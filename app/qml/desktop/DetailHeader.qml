import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Item {
    id: header
    property var music
    property var detail: music.detail
    property string detailKey: detail ? detail.ratingKey : ""
    property bool expanded: false
    implicitHeight: content.implicitHeight + 16
    onDetailKeyChanged: expanded = false
    ColumnLayout {
        id: content; anchors.left: parent.left; anchors.right: parent.right; spacing: 14
        ListView {
            id: photos; Layout.fillWidth: true; Layout.preferredHeight: 260
            orientation: ListView.Horizontal; snapMode: ListView.SnapOneItem; clip: true
            model: header.detail ? header.detail.photos || [] : []
            delegate: Image { width: photos.width; height: photos.height; source: modelData; asynchronous: true; fillMode: Image.PreserveAspectFit }
            Label { anchors.centerIn: parent; visible: photos.count === 0; text: "♪"; color: "#ebad3d"; font.pixelSize: 60 }
            ScrollBar.horizontal: ScrollBar {}
        }
        Label { text:qsTr("Swipe for more photos"); visible:photos.count>1; wrapMode:Text.Wrap; color:"#9eaabd"; Layout.fillWidth:true }
        RowLayout {
            Layout.fillWidth: true
            ColumnLayout {
                Layout.fillWidth: true
                Label { text: header.detail ? header.detail.title : ""; font.pixelSize: 28; font.bold: true; wrapMode: Text.Wrap; Layout.fillWidth: true }
                Label { text: header.detail ? [header.detail.parentTitle || "", header.detail.year > 0 ? header.detail.year : ""].filter(function(value) { return value !== "" }).join(" · ") : ""; color: "#9eaabd"; Layout.fillWidth: true }
            }
            RadioAction { actionName: header.detail ? music.radioActionText(header.detail) : qsTr("Radio"); enabled: !backend.busy; onClicked: music.startRadio(header.detail) }
        }
        RowLayout {
            objectName:"albumActions"; Layout.fillWidth:true; visible:header.detail && header.detail.type==="album"
            Button { objectName:"playTracks"; text:qsTr("Play tracks"); Layout.fillWidth:true; Layout.minimumWidth:0; Layout.preferredWidth:1; enabled:!backend.busy && music.items.length>0; onClicked:music.playAll() }
            Button { objectName:"downloadTracks"; text:qsTr("Download tracks"); Layout.fillWidth:true; Layout.minimumWidth:0; Layout.preferredWidth:1; enabled:!backend.busy && backend.cache.enabled && music.items.length>0; onClicked:music.downloadTracks() }
        }
        Label {
            id: description; Layout.fillWidth: true
            text: header.detail && header.detail.summary ? header.detail.summary : qsTr("No description provided by Plex.")
            textFormat: Text.PlainText; wrapMode: Text.Wrap; color: "#9eaabd"
            maximumLineCount: header.expanded ? 10000 : 6; elide: Text.ElideRight
        }
        Button { text: header.expanded ? qsTr("Read less") : qsTr("Read more"); visible: description.truncated || header.expanded; onClicked: header.expanded = !header.expanded }
        Label { text:qsTr("Similar artists"); visible:header.detail && header.detail.type==="artist"; font.pixelSize:20; color:"#ebad3d" }
        ListView {
            id:related; Layout.fillWidth:true; Layout.preferredHeight:visible ? 160 : 0
            visible:header.detail && header.detail.type==="artist" && music.similarArtists.length>0
            orientation:ListView.Horizontal; clip:true; spacing:12; model:music.similarArtists
            ScrollBar.horizontal:ScrollBar {}
            delegate:ItemDelegate {
                width:150; height:160; enabled:!backend.busy; onClicked:music.activate(modelData,index)
                contentItem:Column {
                    spacing:8
                    Image { width:parent.width; height:100; source:modelData.artwork || ""; asynchronous:true; fillMode:Image.PreserveAspectCrop }
                    Label { width:parent.width; text:modelData.title; maximumLineCount:2; wrapMode:Text.Wrap; elide:Text.ElideRight; color:"#ebad3d" }
                }
            }
        }
        Label {
            Layout.fillWidth:true; visible:header.detail && header.detail.type==="artist" && music.similarArtists.length===0
            text:music.similarState==="loading" ? qsTr("Loading similar artists…") : music.similarState==="unavailable" ? qsTr("Similar artists are unavailable right now.") : qsTr("No similar artists returned by Plex.")
            wrapMode:Text.Wrap; color:"#9eaabd"
        }
        Label { text: header.detail && header.detail.type === "artist" ? qsTr("Albums") : qsTr("Tracks"); font.pixelSize: 22; color: "#ebad3d" }
    }
}
