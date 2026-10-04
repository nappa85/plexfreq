import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    property var music
    SilicaFlickable {
        anchors.fill: parent; contentHeight: column.height
        Column {
            id: column; width: parent.width; spacing: Theme.paddingMedium
            PageHeader { title: qsTr("Connect to Plex") }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: backend.error; visible: text.length > 0; color: Theme.errorColor; wrapMode: Text.Wrap }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Sign in with Plex"); enabled: !backend.busy; onClicked: backend.command("login") }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: qsTr("Complete sign-in in your browser, then return here."); visible: music.polling; wrapMode: Text.Wrap }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Open browser again"); visible: music.polling; onClicked: Qt.openUrlExternally(music.loginUrl) }
            Label {
                x: Theme.horizontalPageMargin; width: parent.width - 2*x
                visible: !!backend.state.signedIn || backend.state.servers.length > 0
                text: qsTr("Choose a Plex server"); color: Theme.highlightColor
                font.pixelSize: Theme.fontSizeLarge; font.bold: true; wrapMode: Text.Wrap
            }
            Label {
                x: Theme.horizontalPageMargin; width: parent.width - 2*x
                visible: !!backend.state.signedIn || backend.state.servers.length > 0
                text: backend.state.servers.length > 0 ? qsTr("Tap a server to open its music libraries.") : qsTr("Refresh the list to find your Plex servers.")
                color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeSmall; wrapMode: Text.Wrap
            }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Refresh servers"); visible: !!backend.state.signedIn; enabled: !backend.busy; onClicked: backend.command("servers") }
            Repeater {
                model: backend.state.servers
                BackgroundItem {
                    id: serverRow
                    x: Theme.horizontalPageMargin; width: column.width - 2*x
                    height: Math.max(Theme.itemSizeLarge, serverDetails.height + 2 * Theme.paddingMedium)
                    enabled: !backend.busy
                    onClicked: { backend.command("select_server", {index:index}); pageStack.pop() }
                    Rectangle {
                        anchors.fill: parent; radius: Theme.paddingSmall
                        color: Theme.highlightColor; opacity: serverRow.highlighted ? 0.25 : 0.12
                    }
                    Column {
                        id: serverDetails
                        anchors.left: parent.left; anchors.leftMargin: Theme.paddingLarge
                        anchors.right: parent.right; anchors.rightMargin: Theme.paddingLarge + Theme.iconSizeSmall
                        anchors.verticalCenter: parent.verticalCenter; spacing: Theme.paddingSmall
                        Label {
                            width: parent.width; text: modelData.name; wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeLarge; font.bold: true; color: Theme.highlightColor
                        }
                        Label {
                            width: parent.width; text: qsTr("Connect to music libraries"); wrapMode: Text.Wrap
                            font.pixelSize: Theme.fontSizeSmall; color: Theme.secondaryColor
                        }
                    }
                    Label {
                        anchors.right: parent.right; anchors.rightMargin: Theme.paddingLarge
                        anchors.verticalCenter: parent.verticalCenter
                        text: "›"; color: Theme.highlightColor; font.pixelSize: Theme.fontSizeExtraLarge
                    }
                }
            }
            SectionHeader { text: qsTr("Direct connection") }
            TextField { id: address; width: parent.width; label: qsTr("Server URL"); placeholderText: "http://192.168.1.10:32400"; text: backend.state.serverUrl || "" }
            PasswordField { id: token; width: parent.width; label: qsTr("Plex token"); placeholderText: "X-Plex-Token" }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Connect"); enabled: !backend.busy && address.text.length > 0; onClicked: { backend.command("connect", {url:address.text, token:token.text}); token.text = ""; pageStack.pop() } }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Sign out"); enabled: !backend.busy; onClicked: { backend.command("logout"); token.text = "" } }
            SectionHeader { text: qsTr("Offline audio cache") }
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Audio settings");onClicked:music.openAudioSettings()}
            TextSwitch {text:qsTr("Autoplay related music when queue ends");checked:!!backend.state.autoplay;enabled:!backend.busy;onClicked:backend.command("autoplay",{enabled:checked})}
            TextSwitch {text:qsTr("Download only on Wi-Fi");checked:!!backend.cache.wifiOnly;onClicked:backend.command("download_policy",{wifi_only:checked,paused:!!backend.cache.paused})}
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Open download manager");onClicked:music.openDownloads()}
            TextSwitch { text: qsTr("Cache queued audio automatically"); checked: backend.cache.enabled; enabled: !backend.busy; onClicked: backend.command("cache_config", {enabled:checked, limit_mb:backend.cache.limitMb, ahead:backend.cache.ahead}) }
            ComboBox {
                width: parent.width; label: qsTr("Cache limit"); value: backend.cache.limitMb + " MiB"; enabled: !backend.busy
                menu: ContextMenu { Repeater { model:[128,256,512,1024,2048]; MenuItem { text:modelData + " MiB"; onClicked: backend.command("cache_config", {enabled:backend.cache.enabled, limit_mb:modelData, ahead:backend.cache.ahead}) } } }
            }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:qsTr("%1 tracks downloaded · %2 MiB used").arg(backend.cache.tracks).arg(Math.round(backend.cache.bytes / 1048576)); wrapMode:Text.Wrap; color:Theme.secondaryColor }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:backend.cache.error; visible:text.length>0; wrapMode:Text.Wrap; color:Theme.errorColor }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:qsTr("The current track and next %1 queued tracks are cached. Download an album before travelling; only completed downloads work offline.").arg(backend.cache.ahead); wrapMode:Text.Wrap; font.pixelSize:Theme.fontSizeExtraSmall; color:Theme.secondaryColor }
            Button { anchors.horizontalCenter:parent.horizontalCenter; text:qsTr("Clear cache and stop playback"); enabled:!backend.busy; onClicked:backend.command("clear_cache") }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: qsTr("PlexFreq 0.1.0 · Direct-stream music player"); color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeExtraSmall; wrapMode: Text.Wrap }
        }
        VerticalScrollDecorator {}
    }
}
