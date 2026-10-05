import QtQuick 2.6
import QtQml.Models 2.1
import QtQuick.Controls 2.15

Menu {
    id: menu
    property var navigation
    property var actionIds: []
    Instantiator {
        model: menu.actionIds
        delegate: MenuItem {
            property var descriptor: menu.navigation.definition(modelData)
            objectName: "nav_" + modelData
            text: descriptor.text
            visible: descriptor.visible
            enabled: descriptor.enabled
            height: visible ? implicitHeight : 0
            checkable: false
            onTriggered: menu.navigation.activate(modelData)
        }
        onObjectAdded: menu.insertItem(index, object)
        onObjectRemoved: menu.removeItem(object)
    }
}
