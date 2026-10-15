import QtQuick 2.6
import QtQml.Models 2.1
import QtQuick.Controls 2.15

// Renders shared ItemActions IDs for one list/grid entry.
Menu {
    id: menu
    property var actions
    property var actionIds: []
    property var item
    property int itemIndex: -1
    Instantiator {
        model: menu.actionIds
        delegate: MenuItem {
            property var descriptor: menu.actions.definition(modelData, menu.item, menu.itemIndex)
            objectName: "item_" + modelData
            text: descriptor.text
            visible: descriptor.visible
            enabled: descriptor.enabled
            height: visible ? implicitHeight : 0
            onTriggered: menu.actions.activate(modelData, menu.item, menu.itemIndex)
        }
        onObjectAdded: menu.insertItem(index, object)
        onObjectRemoved: menu.removeItem(object)
    }
}
