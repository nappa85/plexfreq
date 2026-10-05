import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    id:dialog
    property var music
    title:qsTr("Download manager");standardButtons:Dialog.Close
    contentItem:ColumnLayout {
        ComboBox {Layout.fillWidth:true;model:[30,60,120,240,480];displayText:qsTr("Radio download length")+": "+qsTr("%1 minutes").arg(music.downloadMinutes);onActivated:music.downloadMinutes=model[currentIndex]}
        Switch {text:qsTr("Download only on Wi-Fi");checked:!!backend.cache.wifiOnly;onClicked:backend.command("download_policy",{wifi_only:checked,paused:!!backend.cache.paused})}
        Button {text:backend.cache.paused ? qsTr("Resume downloads") : qsTr("Pause downloads");onClicked:backend.command("download_policy",{wifi_only:!!backend.cache.wifiOnly,paused:!backend.cache.paused})}
        Label {Layout.fillWidth:true;text:backend.cache.waitingForWifi ? qsTr("Waiting for confirmed Wi-Fi; streaming remains available.") : qsTr("%1 MiB audio cache used").arg(Math.round(backend.cache.bytes/1048576));wrapMode:Text.Wrap}
        ListView {Layout.fillWidth:true;Layout.preferredHeight:300;clip:true;model:backend.cache.jobs || [];ScrollBar.vertical:ScrollBar {}
            delegate:ColumnLayout {width:ListView.view.width;spacing:6
                Label {Layout.fillWidth:true;text:modelData.title;elide:Text.ElideRight}
                Label {text:qsTr("Planned audio: %1").arg(music.totalTime(modelData.duration || 0))}
                Label {text:qsTr("%1/%2 tracks · %3 MiB").arg(modelData.ready).arg(modelData.total).arg(Math.round(modelData.bytes/1048576))}
                ProgressBar {Layout.fillWidth:true;from:0;to:Math.max(1,modelData.total);value:modelData.ready}
                Label {visible:modelData.active;text:qsTr("Current track: %1 MiB received").arg(Math.round(backend.cache.received/1048576))}
                Label {Layout.fillWidth:true;text:modelData.error;visible:text.length>0;wrapMode:Text.Wrap;color:"#ff998b"}
                RowLayout {
                    Button {text:qsTr("Play");enabled:modelData.ready>0 && !backend.busy;onClicked:backend.command("play_download",{group:modelData.group})}
                    Button {text:qsTr("Refresh");visible:!!modelData.refreshable;enabled:!backend.busy && !backend.state.offlineMode;onClicked:backend.command("download_action",{group:modelData.group,action:"refresh"})}
                    Button {text:qsTr("Retry");onClicked:backend.command("download_action",{group:modelData.group,action:"retry"})}
                    Button {text:qsTr("Cancel plan");onClicked:backend.command("download_action",{group:modelData.group,action:"cancel"})}
                    Button {text:qsTr("Remove files");onClicked:backend.command("download_action",{group:modelData.group,action:"remove"})}
                }
            }
        }
        Label {text:qsTr("Pending listening history: %1").arg(backend.state.history ? backend.state.history.pending : 0)}
        Button {text:qsTr("Sync history");enabled:!backend.busy && !backend.state.offlineMode;onClicked:backend.command("sync_history")}
        Label {Layout.fillWidth:true;text:backend.state.history ? backend.state.history.error || "" : "";visible:text.length>0;wrapMode:Text.Wrap;color:"#ff998b"}
    }
}
