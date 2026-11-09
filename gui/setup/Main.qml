import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ApplicationWindow {
    id: win
    width: 980
    height: 600
    minimumWidth: 760
    minimumHeight: 480
    visible: true
    title: "Face Unlock"

    SystemPalette { id: pal; colorGroup: SystemPalette.Active }
    color: pal.window

    // brief ok/fail reaction after a finished action
    property string result: ""
    property string resultText: ""
    Timer { id: resultTimer; interval: 1800; onTriggered: win.result = "" }

    Connections {
        target: camera
        function onFinished(what, ok, message) {
            win.result = ok ? "ok" : "fail"
            win.resultText = message
            resultTimer.restart()
        }
    }

    readonly property bool busy: camera.mode !== "preview"
    readonly property string glyphState: result !== "" ? result
                                       : camera.mode === "enroll" || camera.mode === "test" || camera.mode === "saving" ? "scan"
                                       : "idle"

    function hint() {
        if (!camera.faceVisible) return "No face in view"
        if (camera.live && !camera.facing) return "Look into the Kinect"
        if (camera.live) return camera.enrolled > 0 ? "Live 3D face · match " + camera.similarity.toFixed(2) : "Live 3D face"
        switch (camera.why) {
        case "no depth": case "sparse depth": return "No depth here (glass or too close)"
        case "distance": return "Sit 30–130 cm away"
        case "face size": return "Unusual face size"
        case "flat (relief)": case "flat (plane)": return "Looks flat, like a photo"
        default: return "Looks like a bent photo"
        }
    }

    RowLayout {
        anchors.fill: parent
        anchors.margins: 20
        spacing: 24

        // ------------------------------------------------------------ camera preview
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            radius: 14
            color: "#101214"
            clip: true

            Image {
                id: view
                anchors.fill: parent
                fillMode: Image.PreserveAspectFit
                cache: false
                asynchronous: false
                smooth: true
                source: camera.status === "ready" && camera.frame > 0 ? "image://kinect/" + camera.frame : ""

                // face brackets, positioned over the painted image area
                Item {
                    id: bracket
                    readonly property real ox: (view.width - view.paintedWidth) / 2
                    readonly property real oy: (view.height - view.paintedHeight) / 2
                    visible: camera.faceVisible && camera.status === "ready"
                    x: ox + camera.faceRect.x * view.paintedWidth
                    y: oy + camera.faceRect.y * view.paintedHeight
                    width: camera.faceRect.width * view.paintedWidth
                    height: camera.faceRect.height * view.paintedHeight
                    Behavior on x { NumberAnimation { duration: 90 } }
                    Behavior on y { NumberAnimation { duration: 90 } }
                    Behavior on width { NumberAnimation { duration: 90 } }
                    Behavior on height { NumberAnimation { duration: 90 } }
                    readonly property color c: camera.live ? "#3ccf7a" : "#ffb347"

                    Repeater {
                        model: 4
                        Item {
                            readonly property bool isRight: index % 2 === 1
                            readonly property bool isBottom: index >= 2
                            readonly property real len: Math.min(bracket.width, bracket.height) * 0.22
                            x: isRight ? bracket.width - len : 0
                            y: isBottom ? bracket.height - len : 0
                            width: len
                            height: len
                            Rectangle { width: parent.width; height: 3; radius: 1.5; color: bracket.c; y: isBottom ? parent.height - 3 : 0 }
                            Rectangle { width: 3; height: parent.height; radius: 1.5; color: bracket.c; x: isRight ? parent.width - 3 : 0 }
                        }
                    }
                }
            }

            Label {
                anchors.centerIn: parent
                visible: camera.status !== "ready"
                color: "#c8ccd0"
                font.pixelSize: 16
                text: ({
                    starting: "Starting Kinect…",
                    paused: "Waiting for authorization…",
                    nodevice: "Kinect v2 not found.\nIs it plugged into a USB 3 port and powered?",
                    nomodels: "Face models missing in /usr/local/share/kface.\nRun install.sh again."
                })[camera.status] || ""
                horizontalAlignment: Text.AlignHCenter
            }

            // status pill
            Rectangle {
                visible: camera.status === "ready"
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 14
                radius: height / 2
                height: pill.implicitHeight + 12
                width: pill.implicitWidth + 28
                color: Qt.rgba(0, 0, 0, 0.6)
                Label {
                    id: pill
                    anchors.centerIn: parent
                    color: camera.faceVisible ? (camera.live ? "#3ccf7a" : "#ffb347") : "#c8ccd0"
                    text: win.hint()
                }
            }
        }

        // ------------------------------------------------------------ side panel
        ColumnLayout {
            Layout.preferredWidth: 320
            Layout.maximumWidth: 320
            Layout.fillHeight: true
            spacing: 14

            Item { Layout.fillHeight: true }

            Item {
                Layout.alignment: Qt.AlignHCenter
                implicitWidth: 180
                implicitHeight: 180
                Rectangle {
                    anchors.fill: parent
                    radius: width / 2
                    color: "#16191d"
                }
                FaceGlyph {
                    anchors.fill: parent
                    anchors.margins: 14
                    state: win.glyphState
                    progress: camera.mode === "enroll" ? camera.progress : -1
                    accent: pal.highlight
                }
            }

            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: 22
                font.weight: Font.DemiBold
                wrapMode: Text.Wrap
                text: camera.mode === "enroll" ? "Setting up…"
                    : camera.mode === "test" ? "Testing…"
                    : camera.mode === "saving" ? "Saving…"
                    : camera.enrolled > 0 ? "Face Unlock is on" : "Set up Face Unlock"
            }

            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                opacity: 0.8
                text: win.result !== "" ? win.resultText
                    : camera.mode === "enroll" ? "Look at the Kinect and slowly move your head in a small circle."
                    : camera.mode === "test" ? "Look at the Kinect."
                    : camera.mode === "saving" ? "Confirm to save your face data."
                    : camera.enrolled > 0 ? camera.user + " · " + camera.enrolled + " samples"
                    : "Unlock sudo, system prompts and the lock screen with your face. Works in the dark."
            }

            Item { Layout.preferredHeight: 6 }

            Button {
                Layout.fillWidth: true
                highlighted: true
                visible: !win.busy
                enabled: camera.status === "ready"
                text: camera.enrolled > 0 ? "Set up again" : "Set up"
                onClicked: camera.startEnroll()
            }
            Button {
                Layout.fillWidth: true
                visible: !win.busy
                enabled: camera.status === "ready" && camera.enrolled > 0
                text: "Test"
                onClicked: camera.startTest()
            }
            Button {
                Layout.fillWidth: true
                visible: !win.busy
                enabled: camera.status === "ready" && camera.enrolled > 0
                text: "Remove face data"
                onClicked: camera.removeFaces()
            }
            Button {
                Layout.fillWidth: true
                visible: camera.mode === "enroll" || camera.mode === "test"
                text: "Cancel"
                onClicked: camera.cancel()
            }

            Item { Layout.fillHeight: true }

            Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: 11
                opacity: 0.55
                text: "Your password always works as a fallback. Face data stays on this computer in /etc/kface."
            }
        }
    }
}
