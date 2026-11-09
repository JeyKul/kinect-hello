import QtQuick
import QtQuick.Window

Window {
    id: win
    width: 260
    height: 230
    visible: false
    color: "transparent"
    flags: Qt.FramelessWindowHint | Qt.WindowDoesNotAcceptFocus

    property string phase: "scan"

    Connections {
        target: bridge
        function onEventChanged() {
            win.phase = bridge.event
            if (bridge.event === "scan") {
                hideTimer.stop()
                safety.restart()
                card.opacity = 0
                card.scale = 0.9
                win.visible = true
                appear.restart()
            } else if (win.visible) {
                safety.stop()
                hideTimer.restart()
            }
        }
    }

    // fail-safe if the scanner never reports back
    Timer { id: safety; interval: 10000; onTriggered: hideTimer.restart() }
    Timer { id: hideTimer; interval: win.phase === "ok" ? 1100 : 1500; onTriggered: disappear.restart() }

    ParallelAnimation {
        id: appear
        NumberAnimation { target: card; property: "opacity"; to: 1; duration: 180 }
        NumberAnimation { target: card; property: "scale"; to: 1; duration: 260; easing.type: Easing.OutBack }
    }
    SequentialAnimation {
        id: disappear
        ParallelAnimation {
            NumberAnimation { target: card; property: "opacity"; to: 0; duration: 220 }
            NumberAnimation { target: card; property: "scale"; to: 0.94; duration: 220 }
        }
        ScriptAction { script: win.visible = false }
    }

    Rectangle {
        id: card
        anchors.fill: parent
        anchors.margins: 10
        radius: 26
        color: Qt.rgba(0.07, 0.08, 0.1, 0.88)
        border.color: Qt.rgba(1, 1, 1, 0.08)

        FaceGlyph {
            id: glyph
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: 20
            width: 130
            height: 130
            state: win.phase
        }

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: glyph.bottom
            anchors.topMargin: 16
            color: "white"
            font.pixelSize: 15
            font.weight: Font.Medium
            text: win.phase === "ok" ? "Welcome back" : win.phase === "fail" ? "Not recognized" : "Look into the Kinect"
        }
    }
}
