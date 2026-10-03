import QtQuick 2.6
import Sailfish.Silica 1.0

Column {
    id: header
    property var music
    property var detail: music.detail
    property string detailKey: detail ? detail.ratingKey : ""
    property bool expanded: false
    height: implicitHeight
    clip: true
    spacing: Theme.paddingMedium
    onDetailKeyChanged: expanded = false
    ListView {
        id: photos
        width: parent.width; height: Math.min(width * 0.75, Theme.itemSizeExtraLarge * 2.5)
        orientation: ListView.Horizontal; snapMode: ListView.SnapOneItem; clip: true
        model: header.detail ? header.detail.photos || [] : []
        delegate: Item {
            width: photos.width; height: photos.height
            Image { anchors.fill: parent; anchors.margins: Theme.paddingMedium; source: modelData; asynchronous: true; fillMode: Image.PreserveAspectFit }
        }
        Label { anchors.centerIn: parent; visible: photos.count === 0; text: "♪"; color: Theme.highlightColor; font.pixelSize: Theme.fontSizeHuge }
    }
    Label {
        x:Theme.horizontalPageMargin; width:parent.width-2*x
        text:qsTr("Swipe for more photos"); visible:photos.count>1
        color:Theme.secondaryColor; font.pixelSize:Theme.fontSizeExtraSmall; wrapMode:Text.Wrap
    }
    Item {
        x:Theme.horizontalPageMargin; width:parent.width-2*x
        height:Math.max(caption.implicitHeight,radio.height)
        Label {
            id:caption; anchors.left:parent.left; anchors.right:radio.left; anchors.rightMargin:Theme.paddingMedium; anchors.verticalCenter:parent.verticalCenter
            text:header.detail ? [header.detail.parentTitle || "",header.detail.year>0 ? header.detail.year : ""].filter(function(value) { return value!=="" }).join(" · ") : ""
            color:Theme.secondaryColor; wrapMode:Text.Wrap
        }
        RadioAction { id:radio; anchors.right:parent.right; anchors.verticalCenter:parent.verticalCenter; actionName:header.detail ? music.radioActionText(header.detail) : qsTr("Radio"); enabled:!backend.busy; onClicked:music.startRadio(header.detail) }
    }
    Row {
        objectName:"albumActions"
        x:Theme.horizontalPageMargin; width:parent.width-2*x; spacing:Theme.paddingMedium
        visible:header.detail && header.detail.type==="album"
        Button { objectName:"playTracks"; width:(parent.width-parent.spacing)/2; text:qsTr("Play tracks"); enabled:!backend.busy && music.items.length>0; onClicked:music.playAll() }
        Button { objectName:"downloadTracks"; width:(parent.width-parent.spacing)/2; text:qsTr("Download tracks"); enabled:!backend.busy && backend.cache.enabled && music.items.length>0; onClicked:music.downloadTracks() }
    }
    Label {
        id: description
        x: Theme.horizontalPageMargin; width: parent.width - 2*x
        text: header.detail && header.detail.summary ? header.detail.summary : qsTr("No description provided by Plex.")
        textFormat: Text.PlainText; wrapMode: Text.Wrap
        maximumLineCount: header.expanded ? 10000 : 6; elide: Text.ElideRight
        font.pixelSize: Theme.fontSizeSmall; color: Theme.secondaryColor
    }
    Button { anchors.horizontalCenter: parent.horizontalCenter; text: header.expanded ? qsTr("Read less") : qsTr("Read more"); visible: description.truncated || header.expanded; onClicked: header.expanded = !header.expanded }
    SectionHeader { text:qsTr("Similar artists"); visible:header.detail && header.detail.type==="artist" }
    ListView {
        id:related
        width:parent.width; height:visible ? Theme.itemSizeLarge*2 : 0
        visible:header.detail && header.detail.type==="artist" && music.similarArtists.length>0
        orientation:ListView.Horizontal; clip:true; spacing:Theme.paddingMedium
        model:music.similarArtists
        delegate:BackgroundItem {
            width:Theme.itemSizeLarge*1.6; height:related.height
            enabled:!backend.busy; onClicked:music.activate(modelData,index)
            Column {
                anchors.fill:parent; anchors.margins:Theme.paddingSmall; spacing:Theme.paddingSmall
                Image { width:parent.width; height:width*0.8; source:modelData.artwork || ""; asynchronous:true; fillMode:Image.PreserveAspectCrop }
                Label { width:parent.width; text:modelData.title; maximumLineCount:2; wrapMode:Text.Wrap; elide:Text.ElideRight; color:Theme.highlightColor; font.pixelSize:Theme.fontSizeSmall }
            }
        }
    }
    Label {
        x:Theme.horizontalPageMargin; width:parent.width-2*x
        visible:header.detail && header.detail.type==="artist" && music.similarArtists.length===0
        text:music.similarState==="loading" ? qsTr("Loading similar artists…") : music.similarState==="unavailable" ? qsTr("Similar artists are unavailable right now.") : qsTr("No similar artists returned by Plex.")
        wrapMode:Text.Wrap; color:Theme.secondaryColor; font.pixelSize:Theme.fontSizeExtraSmall
    }
    SectionHeader { text: header.detail && header.detail.type === "artist" ? qsTr("Albums") : qsTr("Tracks") }
}
