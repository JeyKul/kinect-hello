// kface-overlay: shows an animated face next to the mouse pointer while kface scans.
// kface verify connects to $XDG_RUNTIME_DIR/kface-overlay.sock and sends "scan", "ok" or "fail".
//
// Wayland clients cannot read the global pointer position, so on each scan we briefly load a
// one-line KWin script that reports workspace.cursorPos back to us over D-Bus. Without KWin the
// overlay falls back to the top centre of the primary screen.
#include "kface_notify.hpp"

#include <LayerShellQt/Window>
#include <QDBusConnection>
#include <QDBusInterface>
#include <QDBusReply>
#include <QFile>
#include <QGuiApplication>
#include <QLocalServer>
#include <QLocalSocket>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickWindow>
#include <QScreen>
#include <QStandardPaths>
#include <QTimer>

#include <unistd.h>

static const char *SERVICE = "org.kde.kface.Overlay";
static const char *SCRIPT_NAME = "kface-cursor";

class Overlay : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.kde.kface.Overlay")
    Q_PROPERTY(QString event MEMBER event_ NOTIFY eventChanged)

public:
    explicit Overlay(QObject *parent = nullptr) : QObject(parent)
    {
        script_ = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation) + "/kface-cursor.js";
        QFile f(script_);
        if (f.open(QIODevice::WriteOnly | QIODevice::Truncate))
            f.write(QString("callDBus(\"%1\", \"/Overlay\", \"%1\", \"cursor\", workspace.cursorPos.x, workspace.cursorPos.y);\n")
                        .arg(SERVICE)
                        .toUtf8());
        auto bus = QDBusConnection::sessionBus();
        bus.registerService(SERVICE);
        bus.registerObject("/Overlay", this, QDBusConnection::ExportScriptableSlots);

        waitCursor_.setSingleShot(true);
        waitCursor_.setInterval(250);
        connect(&waitCursor_, &QTimer::timeout, this, [this] { place(QPoint(), false); });
    }

    ~Overlay() override { QFile::remove(script_); }

    void setWindow(QQuickWindow *w) { win_ = w; }

    void event(const QString &e)
    {
        if (e == "scan") {
            // show once we know where the pointer is (or the wait times out)
            pending_ = true;
            waitCursor_.start();
            requestCursor();
        } else {
            setEvent(e);
        }
    }

public slots:
    Q_SCRIPTABLE void cursor(int x, int y)
    {
        if (pending_) place(QPoint(x, y), true);
    }

signals:
    void eventChanged();

private:
    void setEvent(const QString &e)
    {
        event_ = e;
        emit eventChanged();
    }

    void requestCursor()
    {
        QDBusInterface kwin("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting");
        if (!kwin.isValid()) return;
        kwin.call("unloadScript", SCRIPT_NAME);
        QDBusReply<int> id = kwin.call("loadScript", script_, SCRIPT_NAME);
        if (!id.isValid() || id.value() < 0) return;
        QDBusInterface script("org.kde.KWin", QString("/Scripting/Script%1").arg(id.value()), "org.kde.kwin.Script");
        script.call("run");
        kwin.call("unloadScript", SCRIPT_NAME);
    }

    // Put the card just below the pointer, on the pointer's screen, kept fully on screen.
    void place(QPoint p, bool have)
    {
        waitCursor_.stop();
        pending_ = false;
        auto *ls = LayerShellQt::Window::get(win_);
        QScreen *screen = have ? QGuiApplication::screenAt(p) : nullptr;
        if (!screen) {
            screen = QGuiApplication::primaryScreen();
            QRect g = screen->geometry();
            p = QPoint(g.center().x(), g.top());
        }
        QRect g = screen->geometry();
        int w = win_->width(), h = win_->height();
        int x = std::clamp(p.x() - g.x() - w / 2, 0, std::max(0, g.width() - w));
        int y = have ? p.y() - g.y() + 24 : 40;
        if (y + h > g.height()) y = p.y() - g.y() - h - 24;  // no room below: go above
        y = std::clamp(y, 0, std::max(0, g.height() - h));

        if (qEnvironmentVariableIsSet("KFACE_VERBOSE"))
            qInfo("kface-overlay: pointer %s (%d,%d) -> %s +%d+%d", have ? "at" : "unknown", p.x(), p.y(),
                  qPrintable(screen->name()), x, y);
        if (win_->isVisible() && win_->screen() != screen) win_->hide();
        win_->setScreen(screen);
        ls->setScreen(screen);
        ls->setMargins(QMargins(x, y, 0, 0));
        setEvent("scan");
    }

    QQuickWindow *win_ = nullptr;
    QString event_, script_;
    QTimer waitCursor_;
    bool pending_ = false;
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

    Overlay overlay;
    QObject::connect(&server, &QLocalServer::newConnection, [&] {
        while (QLocalSocket *s = server.nextPendingConnection()) {
            QObject::connect(s, &QLocalSocket::readyRead, s, [s, &overlay] {
                while (s->canReadLine()) {
                    QString e = QString::fromUtf8(s->readLine()).trimmed();
                    if (e == "scan" || e == "ok" || e == "fail") overlay.event(e);
                }
            });
            QObject::connect(s, &QLocalSocket::disconnected, s, &QObject::deleteLater);
        }
    });

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty("bridge", &overlay);
    engine.loadFromModule("KFaceOverlay", "Main");
    auto *win = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (!win) return 1;
    overlay.setWindow(win);

    // small, click-through surface above everything; positioned via margins from the top-left
    win->setFlag(Qt::WindowTransparentForInput);
    if (auto *ls = LayerShellQt::Window::get(win)) {
        ls->setLayer(LayerShellQt::Window::LayerOverlay);
        ls->setAnchors(LayerShellQt::Window::Anchors(LayerShellQt::Window::AnchorTop | LayerShellQt::Window::AnchorLeft));
        ls->setExclusiveZone(-1);
        ls->setKeyboardInteractivity(LayerShellQt::Window::KeyboardInteractivityNone);
        ls->setActivateOnShow(false);
        ls->setWantsToBeOnActiveScreen(false);
        ls->setScope("kface");
    }
    return app.exec();
}

#include "main.moc"
