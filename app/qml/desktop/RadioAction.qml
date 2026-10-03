import QtQuick 2.6
import QtQuick.Controls 2.15
import "../shared"

ToolButton {
    id: action
    property string actionName: qsTr("Radio")
    Accessible.name: actionName
    implicitWidth: 44; implicitHeight: 44
    contentItem: RadioIcon { color: action.palette.highlight; opacity: action.enabled ? 1 : 0.35 }
    ToolTip.visible: hovered
    ToolTip.text: actionName
}
