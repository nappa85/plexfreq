import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    property var music
    property var stats:backend.state.insights || ({plays:0,listenedMs:0,artists:[],albums:[],tracks:[]})
    property int days:30
    property string category:"artists"
    Component.onCompleted:backend.command("listening_insights",{days:days})
    SilicaListView {id:list;anchors.fill:parent;model:stats[category] || []
        header:Column {width:list.width
            PageHeader {title:qsTr("Listening insights")}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("%1 qualified plays · %2 listened").arg(stats.plays).arg(music.totalTime(stats.listenedMs));wrapMode:Text.Wrap}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Based on locally measured qualified plays since insights tracking began.");wrapMode:Text.Wrap;color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall}
            ComboBox {width:parent.width;label:qsTr("Period");value:days ? qsTr("%1 days").arg(days) : qsTr("All history")
                menu:ContextMenu {Repeater {model:[7,30,90,0];MenuItem {text:modelData ? qsTr("%1 days").arg(modelData) : qsTr("All history");onClicked:{days=modelData;backend.command("listening_insights",{days:days})}}}}
            }
            ComboBox {width:parent.width;label:qsTr("Browse");value:category==="artists" ? qsTr("Artists") : category==="albums" ? qsTr("Albums") : qsTr("Tracks")
                menu:ContextMenu {Repeater {model:["artists","albums","tracks"];MenuItem {text:modelData==="artists" ? qsTr("Artists") : modelData==="albums" ? qsTr("Albums") : qsTr("Tracks");onClicked:category=modelData}}}
            }
        }
        delegate:BackgroundItem {width:list.width;height:Theme.itemSizeMedium
            Column {x:Theme.horizontalPageMargin;width:parent.width-2*x;anchors.verticalCenter:parent.verticalCenter
                Label {width:parent.width;text:modelData.title;truncationMode:TruncationMode.Fade}
                Label {width:parent.width;text:qsTr("%1 plays · %2 listened").arg(modelData.plays).arg(music.totalTime(modelData.listenedMs));color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall}
            }
            enabled:false
        }
        VerticalScrollDecorator {}
    }
}
