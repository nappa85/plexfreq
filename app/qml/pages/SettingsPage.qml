import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: settingsPage
    property var music
    // The password is not kept once the page is left.
    onStatusChanged: if (status === PageStatus.Inactive) jellyfinPassword.text = ""
    // Prefilled once: a binding on `text` would replace what the user is
    // typing whenever the backend publishes new state.
    Component.onCompleted: {
        var jellyfin = backend.state.serverKind === "jellyfin"
        address.text = jellyfin ? "" : backend.state.serverUrl || ""
        jellyfinUrl.text = jellyfin ? backend.state.serverUrl || "" : ""
    }
    // Silica switches toggle `checked` themselves on click, dropping the
    // binding; re-sync them whenever the backend publishes new state.
    Connections {target:backend;onStateChanged:autoplay.checked=!!backend.state.autoplay}
    // A completed Jellyfin sign-in (password or Quick Connect) returns to the library.
    Connections {target:backend;onCompleted:if(ok && (op==="jellyfin_login" || (op==="jellyfin_quick_connect_poll" && data.signedIn)) && pageStack.currentPage === settingsPage)pageStack.pop()}
    Connections {target:backend;onCacheChanged:{wifiOnly.checked=!!backend.cache.wifiOnly;cacheEnabled.checked=!!backend.cache.enabled}}
    SilicaFlickable {
        anchors.fill: parent; contentHeight: column.height
        Column {
            id: column; width: parent.width; spacing: Theme.paddingMedium
            PageHeader { title: qsTr("Connect to a server") }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: backend.error; visible: text.length > 0; color: Theme.errorColor; wrapMode: Text.Wrap }
            ComboBox {
                id: serverType; width: parent.width; label: qsTr("Server type"); value: music.signInKind === "jellyfin" ? "Jellyfin" : "Plex"; enabled: !backend.busy && !music.quickConnectPending
                menu: ContextMenu {
                    MenuItem { text: "Plex"; onClicked: music.signInChoice = "plex" }
                    MenuItem { text: "Jellyfin"; onClicked: music.signInChoice = "jellyfin" }
                }
            }
            Column {
                id: plexSection; objectName: "plexSignIn"; width: parent.width; spacing: Theme.paddingMedium; visible: music.signInKind === "plex"
                Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Sign in with Plex"); enabled: !backend.busy; onClicked: backend.command("login") }
                Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: qsTr("Complete sign-in in your browser, then return here."); visible: music.polling; wrapMode: Text.Wrap }
                Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Open browser again"); visible: music.polling; onClicked: Qt.openUrlExternally(music.loginUrl) }
                Label {
                    x: Theme.horizontalPageMargin; width: parent.width - 2*x
                    visible: (!!backend.state.signedIn && !music.jellyfin) || backend.state.servers.length > 0
                    text: qsTr("Choose a Plex server"); color: Theme.highlightColor
                    font.pixelSize: Theme.fontSizeLarge; font.bold: true; wrapMode: Text.Wrap
                }
                Label {
                    x: Theme.horizontalPageMargin; width: parent.width - 2*x
                    visible: (!!backend.state.signedIn && !music.jellyfin) || backend.state.servers.length > 0
                    text: backend.state.servers.length > 0 ? qsTr("Tap a server to open its music libraries.") : qsTr("Refresh the list to find your Plex servers.")
                    color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeSmall; wrapMode: Text.Wrap
                }
                Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Refresh servers"); visible: !!backend.state.signedIn && !music.jellyfin; enabled: !backend.busy; onClicked: backend.command("servers") }
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
                TextField { id: address; width: parent.width; label: qsTr("Server URL"); placeholderText: "http://192.168.1.10:32400" }
                PasswordField { id: token; width: parent.width; label: qsTr("Plex token"); placeholderText: "X-Plex-Token" }
                Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Connect"); enabled: !backend.busy && address.text.length > 0; onClicked: { backend.command("connect", {url:address.text, token:token.text}); token.text = ""; pageStack.pop() } }
            }
            Column {
                id: jellyfinSection; objectName: "jellyfinSignIn"; width: parent.width; spacing: Theme.paddingMedium; visible: music.signInKind === "jellyfin"
                TextField { id: jellyfinUrl; objectName: "jellyfinUrl"; width: parent.width; label: qsTr("Server URL"); placeholderText: "https://host/jellyfin"; inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoPredictiveText; enabled: !music.quickConnectPending }
                TextField { id: jellyfinUser; objectName: "jellyfinUser"; width: parent.width; label: qsTr("Username"); inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText; enabled: !music.quickConnectPending }
                PasswordField { id: jellyfinPassword; objectName: "jellyfinPassword"; width: parent.width; label: qsTr("Password"); enabled: !music.quickConnectPending }
                Button {
                    objectName: "jellyfinSignInButton"; anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Sign in to Jellyfin")
                    enabled: !backend.busy && !music.quickConnectPending && jellyfinUrl.text.length > 0 && jellyfinUser.text.length > 0
                    onClicked: { music.jellyfinSignIn(jellyfinUrl.text, jellyfinUser.text, jellyfinPassword.text); jellyfinPassword.text = "" }
                }
                Button {
                    objectName: "jellyfinQuickConnectButton"; anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Use Quick Connect")
                    visible: !music.quickConnectPending; enabled: !backend.busy && jellyfinUrl.text.length > 0
                    onClicked: { music.startQuickConnect(jellyfinUrl.text); jellyfinPassword.text = "" }
                }
                Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; visible: music.quickConnectPending; text: qsTr("Enter this code in Jellyfin under Quick Connect, then wait here."); wrapMode: Text.Wrap }
                Label { objectName: "quickConnectCode"; x: Theme.horizontalPageMargin; width: parent.width - 2*x; visible: music.quickConnectPending; text: music.quickConnectCode; color: Theme.highlightColor; font.pixelSize: Theme.fontSizeHuge; font.bold: true; horizontalAlignment: Text.AlignHCenter }
                Button { objectName: "quickConnectCancel"; anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Cancel"); visible: music.quickConnectPending; onClicked: music.cancelQuickConnect() }
            }
            Button { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Sign out"); enabled: !backend.busy; onClicked: { backend.command("logout"); token.text = ""; jellyfinPassword.text = "" } }
            SectionHeader { text: qsTr("Offline audio cache") }
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Audio settings");onClicked:music.openAudioSettings()}
            TextSwitch {id:autoplay;text:qsTr("Autoplay related music when queue ends");checked:!!backend.state.autoplay;enabled:!backend.busy;onClicked:backend.command("autoplay",{enabled:checked})}
            TextSwitch {id:wifiOnly;text:qsTr("Download only on Wi-Fi");checked:!!backend.cache.wifiOnly;onClicked:backend.command("download_policy",{wifi_only:checked,paused:!!backend.cache.paused})}
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Open download manager");onClicked:music.openDownloads()}
            TextSwitch { id: cacheEnabled; text: qsTr("Cache queued audio automatically"); checked: !!backend.cache.enabled; enabled: !backend.busy; onClicked: backend.command("cache_config", {enabled:checked, limit_mb:backend.cache.limitMb, ahead:backend.cache.ahead}) }
            ComboBox {
                width: parent.width; label: qsTr("Cache limit"); value: backend.cache.limitMb + " MiB"; enabled: !backend.busy
                menu: ContextMenu { Repeater { model:[128,256,512,1024,2048]; MenuItem { text:modelData + " MiB"; onClicked: backend.command("cache_config", {enabled:backend.cache.enabled, limit_mb:modelData, ahead:backend.cache.ahead}) } } }
            }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:qsTr("%1 tracks downloaded · %2 MiB used").arg(backend.cache.tracks).arg(Math.round(backend.cache.bytes / 1048576)); wrapMode:Text.Wrap; color:Theme.secondaryColor }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:backend.cache.error; visible:text.length>0; wrapMode:Text.Wrap; color:Theme.errorColor }
            Label { x:Theme.horizontalPageMargin; width:parent.width-2*x; text:qsTr("The current track and next %1 queued tracks are cached. Download an album before travelling; only completed downloads work offline.").arg(backend.cache.ahead); wrapMode:Text.Wrap; font.pixelSize:Theme.fontSizeExtraSmall; color:Theme.secondaryColor }
            Button { anchors.horizontalCenter:parent.horizontalCenter; text:qsTr("Clear cache and stop playback"); enabled:!backend.busy; onClicked:backend.command("clear_cache") }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: qsTr("PlexFreq %1 · Direct-stream music player").arg(Qt.application.version); color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeExtraSmall; wrapMode: Text.Wrap }
        }
        VerticalScrollDecorator {}
    }
}
