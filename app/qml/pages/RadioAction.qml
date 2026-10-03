import QtQuick 2.6
import Sailfish.Silica 1.0
import "../shared"

IconButton {
    id: action
    property string actionName: qsTr("Radio")
    Accessible.name: actionName
    width: Theme.itemSizeSmall; height: width
    icon.source: ""
    RadioIcon {
        anchors.centerIn: parent; width: Theme.iconSizeMedium; height: width
        color: action.highlighted ? Theme.highlightColor : Theme.primaryColor
        opacity: action.enabled ? 1 : 0.35
    }
}
