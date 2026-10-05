import QtQuick 2.6

Item {
    property alias music: session
    property alias rootView: root
    property alias artistView: artist
    property alias albumView: album
    Session { id: session; section: "1" }
    BrowsePageView { id: root; music: session }
    BrowsePageView { id: artist; music: session; pageKind: "artist"; pageKey: "10" }
    BrowsePageView { id: album; music: session; pageKind: "album"; pageKey: "20" }
}
