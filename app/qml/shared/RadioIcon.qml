import QtQuick 2.6

Item {
    id: icon
    property color color: "white"
    implicitWidth: 32; implicitHeight: 32
    clip: true
    // Scene-graph geometry avoids a resized Canvas backing texture/context.
    Item {
        width: 32; height: 32; anchors.centerIn: parent
        scale: Math.min(icon.width, icon.height) / 32
        Rectangle { x: 14; y: 8; width: 4; height: 4; radius: 2; color: icon.color; antialiasing: true }
        Rectangle { x: 15; y: 11; width: 2; height: 20; rotation: 20; transformOrigin: Item.Top; color: icon.color; antialiasing: true }
        Rectangle { x: 15; y: 11; width: 2; height: 20; rotation: -20; transformOrigin: Item.Top; color: icon.color; antialiasing: true }
        Rectangle { x: 11; y: 25; width: 10; height: 2; color: icon.color; antialiasing: true }
        Repeater {
            model: [6, 11]
            Item {
                property real radius: modelData
                x: 16 - radius - 1; y: 10 - radius * 0.75
                width: radius - 2; height: radius * 1.5; clip: true
                Rectangle { x: 1; y: -parent.radius * 0.25; width: parent.radius * 2; height: width; radius: width / 2; color: "transparent"; border.color: icon.color; border.width: 2; antialiasing: true }
            }
        }
        Repeater {
            model: [6, 11]
            Item {
                property real radius: modelData
                x: 19; y: 10 - radius * 0.75
                width: radius - 2; height: radius * 1.5; clip: true
                Rectangle { x: -parent.radius - 3; y: -parent.radius * 0.25; width: parent.radius * 2; height: width; radius: width / 2; color: "transparent"; border.color: icon.color; border.width: 2; antialiasing: true }
            }
        }
    }
}
