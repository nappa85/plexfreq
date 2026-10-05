import QtQuick 2.6

Item {
    id: link
    property string text: ""
    property color color: "#ebad3d"
    property color pressedColor: color
    property real fontSize: 14
    property string fontFamily: ""
    property real minimumTouchHeight: 0
    implicitWidth: caption.implicitWidth
    implicitHeight: Math.max(caption.implicitHeight, minimumTouchHeight)
    opacity: enabled ? 1 : 0.4
    activeFocusOnTab: true
    signal clicked()
    Accessible.role: Accessible.Link
    Accessible.name: text
    Accessible.onPressAction: if (link.enabled) link.clicked()
    Text {
        id: caption
        anchors.left: parent.left; anchors.verticalCenter: parent.verticalCenter
        width: parent.width
        text: link.text
        color: touch.pressed || link.activeFocus ? link.pressedColor : link.color
        font.pixelSize: link.fontSize; font.family: link.fontFamily; font.underline: true
        elide: Text.ElideRight
    }
    MouseArea {
        id: touch
        anchors.fill: parent
        enabled: link.enabled
        cursorShape: Qt.PointingHandCursor
        onPressed: link.forceActiveFocus()
        onClicked: if (link.enabled) link.clicked()
    }
    Keys.onPressed: {
        if (link.enabled && (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space)) {
            link.clicked()
            event.accepted = true
        }
    }
}
