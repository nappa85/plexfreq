import QtQuick 2.6
import "qrc:/qml/shared"

// A plain ListView with a header and a WindowedGrid footer, wired like the
// library pages, that records which grid delegates are alive.
Item {
    id: root
    width: 400
    height: 600
    property int live: 0
    property int maxLive: 0
    property var liveIndices: ({})
    property int headerHeight: 100
    property int cols: 3
    property real cell: 100
    property alias list: list
    property var grid: list.footerItem ? list.footerItem.grid : null
    property alias position: position
    property alias rows: rows
    property int countChanges: 0
    // The library lists hold the spacer row while their content is the grid footer;
    // false is the control that shows what the list does without it.
    property bool useSpacer: true
    signal resetView()
    Connections { target: root; onResetView: list.positionViewAtBeginning() }
    GridSpacer { id: gridSpacer }

    ListModel { id: rows }
    function fill(n) {
        rows.clear()
        for (var i = 0; i < n; i++)
            rows.append({title: "Artist " + i})
    }
    function append(n) {
        var base = rows.count
        for (var i = 0; i < n; i++)
            rows.append({title: "Artist " + (base + i)})
    }
    function has(index) {
        return liveIndices[index] > 0
    }
    function topIndex() {
        var y = Math.max(0, grid.mapFromItem(list, 0, 0).y)
        return Math.min(grid.count - 1, Math.floor(y / grid.cellHeight) * grid.columns)
    }
    function bottomIndex() {
        var y = Math.max(0, grid.mapFromItem(list, 0, list.height - 1).y)
        return Math.min(grid.count - 1, Math.floor(y / grid.cellHeight) * grid.columns + grid.columns - 1)
    }
    // Every cell in the viewport has a delegate.
    function viewportFilled() {
        if (grid.count === 0)
            return true
        for (var i = topIndex(); i <= bottomIndex(); i++)
            if (!has(i))
                return false
        return true
    }

    GridPosition {
        id: position
        listView: list
        artistBrowse: true
        alphabet: [{letter: "A", offset: 0}, {letter: "B", offset: 600}, {letter: "C", offset: 999}]
    }
    ListView {
        id: list
        objectName: "fixtureList"
        width: root.width
        height: root.height
        spacing: 5
        model: root.useSpacer ? gridSpacer : null
        delegate: Item {
            property bool spacer: entry.type === "spacer"
            width: list.width
            height: spacer ? 0 : 76
            visible: !spacer
            enabled: !spacer
        }
        header: Item { width: list.width; height: root.headerHeight }
        footer: Item {
            width: list.width
            property alias grid: grid
            property int gridCount: grid.count
            height: grid.height
            WindowedGrid {
                id: grid
                objectName: "routeGrid"
                listView: list
                width: parent.width
                visible: true
                model: rows
                onCountChanged: { root.countChanges++; position.relayout() }
                onColumnsChanged: position.relayout()
                onHeightChanged: position.relayout()
                columns: root.cols
                cellWidth: width / columns
                cellHeight: root.cell
                delegate: Rectangle {
                    property int cellIndex: index
                    property int created: -1
                    width: grid.cellWidth
                    height: grid.cellHeight
                    color: "gray"
                    Component.onCompleted: {
                        created = cellIndex
                        root.liveIndices[created] = (root.liveIndices[created] || 0) + 1
                        root.live++
                        root.maxLive = Math.max(root.maxLive, root.live)
                    }
                    Component.onDestruction: {
                        // A replaced delegate can be destroyed after its successor was created.
                        if (--root.liveIndices[created] <= 0)
                            delete root.liveIndices[created]
                        root.live--
                    }
                }
            }
        }
    }
}
