import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    property var music
    SilicaListView {id:list;anchors.fill:parent;model:backend.cache.jobs || []
        header:Column {width:list.width;spacing:Theme.paddingSmall
            PageHeader {title:qsTr("Download manager")}
            TextSwitch {text:qsTr("Download only on Wi-Fi");checked:!!backend.cache.wifiOnly;onClicked:backend.command("download_policy",{wifi_only:checked,paused:!!backend.cache.paused})}
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:backend.cache.paused ? qsTr("Resume downloads") : qsTr("Pause downloads");onClicked:backend.command("download_policy",{wifi_only:!!backend.cache.wifiOnly,paused:!backend.cache.paused})}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:backend.cache.waitingForWifi ? qsTr("Waiting for a confirmed Wi-Fi connection. Streaming remains available.") : qsTr("%1 MiB audio cache used").arg(Math.round(backend.cache.bytes/1048576));wrapMode:Text.Wrap;color:Theme.secondaryColor}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Pending listening history: %1").arg(backend.state.history ? backend.state.history.pending : 0);wrapMode:Text.Wrap}
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Sync listening history");enabled:!backend.busy && !backend.state.offlineMode;onClicked:backend.command("sync_history")}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:backend.state.history ? backend.state.history.error || "" : "";visible:text.length>0;wrapMode:Text.Wrap;color:Theme.errorColor}
        }
        delegate:ListItem {
            width:list.width;contentHeight:info.height+2*Theme.paddingMedium
            Column {id:info;x:Theme.horizontalPageMargin;width:parent.width-2*x;y:Theme.paddingMedium;spacing:Theme.paddingSmall
                Label {width:parent.width;text:modelData.title;truncationMode:TruncationMode.Fade}
                Label {width:parent.width;text:qsTr("%1/%2 tracks · %3 MiB").arg(modelData.ready).arg(modelData.total).arg(Math.round(modelData.bytes/1048576))+(modelData.active?" · "+qsTr("Downloading"):"");font.pixelSize:Theme.fontSizeExtraSmall;color:Theme.secondaryColor}
                ProgressBar {width:parent.width;minimumValue:0;maximumValue:Math.max(1,modelData.total);value:modelData.ready}
                Label {width:parent.width;visible:modelData.active;text:qsTr("Current track: %1 MiB received").arg(Math.round(backend.cache.received/1048576));font.pixelSize:Theme.fontSizeExtraSmall;color:Theme.secondaryColor}
                Label {width:parent.width;text:modelData.error;visible:text.length>0;wrapMode:Text.Wrap;color:Theme.errorColor;font.pixelSize:Theme.fontSizeExtraSmall}
            }
            menu:ContextMenu {
                MenuItem {text:qsTr("Retry/resume");onClicked:backend.command("download_action",{group:modelData.group,action:"retry"})}
                MenuItem {text:qsTr("Cancel plan / keep files");onClicked:backend.command("download_action",{group:modelData.group,action:"cancel"})}
                MenuItem {text:qsTr("Remove download");onClicked:backend.command("download_action",{group:modelData.group,action:"remove"})}
            }
        }
        VerticalScrollDecorator {}
    }
}
