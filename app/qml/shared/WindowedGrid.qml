import QtQuick 2.6

// Artist grid for a list footer that instantiates only the delegates near the
// list viewport. The item itself keeps the full grid height (rows * cellHeight)
// so the surrounding list, its scroll bar and GridPosition see the whole grid;
// a non-interactive inner GridView, one viewport tall plus a margin, is moved
// along the list so its cells are the ones in view. Qt 5.6 GridView cannot
// reuse delegates, so delegates are created and destroyed at the window edges;
// artwork comes from the application's image provider (image://plexfreq),
// which caches it, so scrolling back does not refetch from the server.
//
// The item is the footer's `grid` for GridPosition: it exposes `count`,
// `columns`, `cellWidth`, `cellHeight` and `height`, and is the geometry
// parent of the cells (mapToItem/mapFromItem). The owner sets `listView`,
// `model`, `delegate`, `columns`, `cellWidth` and `cellHeight`, and may react
// to countChanged/columnsChanged/heightChanged on the instance.
Item {
    id: root
    property var listView: null
    property var model: null
    property Component delegate
    property int columns: 1
    property real cellWidth: width / Math.max(1, columns)
    property real cellHeight: 100
    // Height kept ready beyond the list viewport on each side: the window
    // margin and the GridView cacheBuffer are one row each.
    property real marginHeight: cellHeight
    // Model size (0 while the model is released).
    property int count: view.count
    // True while a delegate's context menu is open: the window must not move
    // away from the delegate that owns the menu.
    property bool holdWindow: false
    // Window position and height inside this item.
    property real windowOffset: 0
    property real windowHeight: 0
    property bool syncing: false
    property string layoutKey: ""
    property bool rebuilding: false
    property real heldHeight: 0
    height: fullHeight()
    clip: false

    function fullHeight() {
        if (rebuilding)
            return heldHeight
        return visible && columns > 0 ? Math.ceil(view.count / columns) * cellHeight : 0
    }
    function clamp(value, low, high) {
        return Math.max(low, Math.min(value, high))
    }
    // Top of this item in the list content coordinates.
    function contentTop() {
        return listView && listView.contentItem ? root.mapToItem(listView.contentItem, 0, 0).y : 0
    }
    // Not a binding: the offset depends on mapToItem, which is not reactive.
    function sync() {
        if (syncing)
            return
        syncing = true
        var startCount = view.count
        var total = fullHeight()
        if (holdWindow && (!visible || view.count === 0))
            holdWindow = false
        var win = windowHeight
        var offset = windowOffset
        if (!holdWindow) {
            win = listView ? Math.min(listView.height + 2 * marginHeight, total) : total
            offset = clamp(listView ? listView.contentY - contentTop() - marginHeight : 0, 0, Math.max(0, total - win))
        } else {
            win = Math.min(win, total)
            offset = clamp(offset, 0, Math.max(0, total - win))
        }
        windowHeight = win
        windowOffset = offset
        view.height = win
        view.y = offset
        // A GridView whose cells change size while it sits far from its start
        // keeps its delegates laid out for the old geometry, so it is rebuilt
        // (see rebuildLater) once the geometry has settled.
        var key = columns + ":" + cellWidth + ":" + cellHeight
        if (layoutKey !== key) {
            layoutKey = key
            rebuildLater.restart()
        }
        view.contentY = offset
        syncing = false
        // A model reset can change the count while the window moves; its
        // change handler was suppressed above, so place the window again.
        if (view.count !== startCount)
            sync()
    }

    // Re-assigning the model resets the GridView so it lays out for the current
    // geometry. The item keeps its height meanwhile: the list would otherwise
    // shrink with the empty model and clamp its scroll position. Deferred, so
    // it does not run inside the binding evaluation that changed the geometry.
    Timer {
        id: rebuildLater
        interval: 0
        onTriggered: {
            heldHeight = fullHeight()
            rebuilding = true
            view.model = null
            view.model = Qt.binding(function() { return root.model })
            rebuilding = false
            sync()
        }
    }

    onHoldWindowChanged: sync()
    Component.onCompleted: sync()

    Connections {
        target: root
        onVisibleChanged: root.sync()
        onCountChanged: root.sync()
        onColumnsChanged: root.sync()
        onModelChanged: root.sync()
        onCellHeightChanged: root.sync()
        onHeightChanged: root.sync()
        onYChanged: root.sync()
        onListViewChanged: root.sync()
    }
    Connections {
        target: root.listView
        onContentYChanged: root.sync()
        onHeightChanged: root.sync()
        onContentHeightChanged: root.sync()
    }
    // The footer moves when the header or the rows above it change size.
    Connections {
        target: root.parent
        onYChanged: root.sync()
    }

    GridView {
        id: view
        x: 0
        width: root.width
        interactive: false
        clip: false
        // No current item: GridView would keep delegate 0 alive far from the window.
        currentIndex: -1
        model: root.model
        delegate: root.delegate
        cellWidth: root.cellWidth
        cellHeight: root.cellHeight
        cacheBuffer: Math.max(0, Math.round(root.marginHeight))
        // Model resets and delegate creation move contentY; keep it on the window.
        onContentYChanged: if (!root.syncing && contentY !== root.windowOffset) root.sync()
        onCountChanged: root.sync()
    }
}
