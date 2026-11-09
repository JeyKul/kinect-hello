// kface-overlay: shows an animated face at the top of the screen while kface scans.
// kface verify connects to $XDG_RUNTIME_DIR/kface-overlay.sock and sends "scan", "ok" or "fail".
#include "kface_notify.hpp"

#include <LayerShellQt/Window>
#include <QGuiApplication>
#include <QLocalServer>
#include <QLocalSocket>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickWindow>

#include <unistd.h>

class Bridge : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString event MEMBER event_ NOTIFY eventChanged)
public:
    void set(const QString &e)
    {
        event_ = e;
        emit eventChanged();
    }
signals:
    void eventChanged();

private:
    QString event_;
};

int main(int argc, char **argv)
{
    QGuiApplication app(argc, argv);
    app.setApplicationName("kface-overlay");
    app.setQuitOnLastWindowClosed(false);

    QString path = QString::fromStdString(kface::overlay_socket(getuid()));
    {
        QLocalSocket probe;
        probe.connectToServer(path);
        if (probe.waitForConnected(200)) return 0;  // already running
    }
    QLocalServer::removeServer(path);
    QLocalServer server;
    server.setSocketOptions(QLocalServer::UserAccessOption);
    if (!server.listen(path)) {
        qWarning("kface-overlay: cannot listen on %s", qPrintable(path));
        return 1;
    }

    Bridge bridge;
    QObject::connect(&server, &QLocalServer::newConnection, [&] {
        while (QLocalSocket *s = server.nextPendingConnection()) {
            QObject::connect(s, &QLocalSocket::readyRead, s, [s, &bridge] {
                while (s->canReadLine()) {
                    QString e = QString::fromUtf8(s->readLine()).trimmed();
                    if (e == "scan" || e == "ok" || e == "fail") bridge.set(e);
                }
            });
            QObject::connect(s, &QLocalSocket::disconnected, s, &QObject::deleteLater);
        }
    });

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty("bridge", &bridge);
    engine.loadFromModule("KFaceOverlay", "Main");
    auto *win = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (!win) return 1;

    // a small, click-through surface above everything, centred at the top
    win->setFlag(Qt::WindowTransparentForInput);
    if (auto *ls = LayerShellQt::Window::get(win)) {
        ls->setLayer(LayerShellQt::Window::LayerOverlay);
        ls->setAnchors(LayerShellQt::Window::AnchorTop);
        ls->setMargins(QMargins(0, 40, 0, 0));
        ls->setExclusiveZone(-1);
        ls->setKeyboardInteractivity(LayerShellQt::Window::KeyboardInteractivityNone);
        ls->setActivateOnShow(false);
        ls->setScope("kface");
    }
    return app.exec();
}

#include "main.moc"
