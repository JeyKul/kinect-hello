// Animated face glyph: a ring of ticks around a minimal face.
//   state "idle"  - calm, occasional blink
//   state "scan"  - a bright sweep runs around the ring, eyes glance around
//   state "ok"    - ring fills green, mouth turns into a smile
//   state "fail"  - ring turns red, mouth flattens, glyph shakes
// progress (0..1, or < 0 for none) lights ticks clockwise, used while enrolling.
import QtQuick
import QtQuick.Shapes

Item {
    id: root
    property string state: "idle"
    property real progress: -1
    property color accent: "#4aa3ff"
    property color okColor: "#3ccf7a"
    property color failColor: "#ff5a5f"
    property color dim: Qt.rgba(1, 1, 1, 0.18)
    property color faceColor: "white"

    readonly property color tint: state === "ok" ? okColor : state === "fail" ? failColor : accent
    readonly property real s: Math.min(width, height)

    implicitWidth: 160
    implicitHeight: 160
    transform: Translate { id: shakeT }

    // ---------------------------------------------------------------- ring
    Item {
        id: ring
        anchors.centerIn: parent
        width: root.s
        height: root.s

        property real sweep: 0
        NumberAnimation on sweep {
            from: 0; to: 1; duration: 1400
            loops: Animation.Infinite
            running: root.state === "scan"
        }

        Repeater {
            model: 40
            Rectangle {
                readonly property real a: index / 40
                // distance of this tick behind the sweep head, 0..1
                readonly property real lag: ((ring.sweep - a) % 1 + 1) % 1
                readonly property bool lit: root.state === "ok" || root.state === "fail"
                                            || (root.progress >= 0 && a < root.progress)
                width: root.s * 0.022
                height: root.s * 0.085
                radius: width / 2
                x: ring.width / 2 - width / 2
                y: 0
                color: lit ? root.tint
                           : root.state === "scan" ? Qt.tint(root.dim, Qt.rgba(root.accent.r, root.accent.g, root.accent.b,
                                                                              Math.max(0, 1 - lag * 3)))
                           : root.dim
                transform: Rotation {
                    origin.x: width / 2
                    origin.y: ring.height / 2
                    angle: a * 360
                }
                Behavior on color { enabled: !(root.state === "scan"); ColorAnimation { duration: 250 } }
            }
        }
    }

    // ---------------------------------------------------------------- face
    Item {
        id: face
        anchors.centerIn: parent
        width: root.s * 0.5
        height: width

        property real look: 0      // -1 .. 1 horizontal glance
        property real blink: 1     // eye openness
        property real smile: root.state === "ok" ? 1 : root.state === "fail" ? -0.35 : 0.15
        Behavior on smile { NumberAnimation { duration: 350; easing.type: Easing.OutBack } }

        SequentialAnimation on look {
            running: root.state === "scan"
            loops: Animation.Infinite
            NumberAnimation { to: -1; duration: 380; easing.type: Easing.InOutQuad }
            PauseAnimation { duration: 260 }
            NumberAnimation { to: 1; duration: 520; easing.type: Easing.InOutQuad }
            PauseAnimation { duration: 260 }
            NumberAnimation { to: 0; duration: 380; easing.type: Easing.InOutQuad }
            PauseAnimation { duration: 400 }
        }
        Connections {
            target: root
            function onStateChanged() { if (root.state !== "scan") face.look = 0 }
        }

        SequentialAnimation on blink {
            loops: Animation.Infinite
            running: root.state !== "fail"
            PauseAnimation { duration: 2600 }
            NumberAnimation { to: 0.1; duration: 80 }
            NumberAnimation { to: 1; duration: 120 }
        }

        Repeater {
            model: 2
            Rectangle {
                width: face.width * 0.13
                height: face.width * 0.2 * face.blink
                radius: width / 2
                color: root.faceColor
                x: face.width * (index === 0 ? 0.28 : 0.72) - width / 2 + face.look * face.width * 0.06
                y: face.height * 0.36 - height / 2
            }
        }

        Shape {
            anchors.fill: parent
            preferredRendererType: Shape.CurveRenderer
            ShapePath {
                strokeColor: root.faceColor
                strokeWidth: face.width * 0.075
                fillColor: "transparent"
                capStyle: ShapePath.RoundCap
                startX: face.width * 0.24
                startY: face.height * 0.7
                PathQuad {
                    controlX: face.width * 0.5
                    controlY: face.height * (0.7 + 0.24 * face.smile)
                    x: face.width * 0.76
                    y: face.height * 0.7
                }
            }
        }
    }

    // ---------------------------------------------------------------- reactions
    SequentialAnimation {
        id: shake
        NumberAnimation { target: shakeT; property: "x"; to: -10; duration: 50 }
        NumberAnimation { target: shakeT; property: "x"; to: 10; duration: 80 }
        NumberAnimation { target: shakeT; property: "x"; to: -7; duration: 80 }
        NumberAnimation { target: shakeT; property: "x"; to: 4; duration: 70 }
        NumberAnimation { target: shakeT; property: "x"; to: 0; duration: 60 }
    }
    SequentialAnimation {
        id: pop
        NumberAnimation { target: root; property: "scale"; to: 1.08; duration: 140; easing.type: Easing.OutQuad }
        NumberAnimation { target: root; property: "scale"; to: 1.0; duration: 260; easing.type: Easing.OutBack }
    }
    onStateChanged: {
        if (state === "fail") shake.restart()
        else if (state === "ok") pop.restart()
    }
}
