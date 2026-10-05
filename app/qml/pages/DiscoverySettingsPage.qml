import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    property var music
    SilicaListView {id:list;anchors.fill:parent;model:backend.state.discoveryHubs || []
        header:PageHeader {title:qsTr("Customize discovery")}
        delegate:ListItem {width:list.width;contentHeight:Theme.itemSizeMedium
            TextSwitch {width:parent.width;text:modelData.title;checked:!modelData.hidden;enabled:!backend.busy;onClicked:music.toggleHub(modelData.identifier,!checked)}
            menu:ContextMenu {MenuItem {text:qsTr("Move up");enabled:index>0 && !backend.busy;onClicked:music.moveHub(index,-1)}MenuItem {text:qsTr("Move down");enabled:index+1<list.count && !backend.busy;onClicked:music.moveHub(index,1)}}
        }
        VerticalScrollDecorator {}
    }
}
