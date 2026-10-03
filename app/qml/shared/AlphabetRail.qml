import QtQuick 2.6

Item {
    id: rail
    property var groups: []
    property color color: "white"
    property color highlightColor: "#ebad3d"
    property real fontSize: 16
    property string selected: ""
    signal chosen(string letter)
    function selectAt(y) {
        if (!groups.length || height<=0) return
        var index=Math.max(0,Math.min(groups.length-1,Math.floor(y*groups.length/height)))
        selected=groups[index].letter
    }
    Column {
        width:parent.width
        Repeater {
            model:rail.groups
            Text {
                width:rail.width; height:rail.groups.length ? rail.height/rail.groups.length : 0
                text:modelData.letter; color:text===rail.selected ? rail.highlightColor : rail.color
                font.pixelSize:Math.min(rail.fontSize,height*0.78); font.bold:text===rail.selected
                horizontalAlignment:Text.AlignHCenter; verticalAlignment:Text.AlignVCenter
            }
        }
    }
    MouseArea {
        anchors.fill:parent
        onPressed:rail.selectAt(mouse.y)
        onPositionChanged:if (pressed) rail.selectAt(mouse.y)
        onReleased:if (rail.selected) rail.chosen(rail.selected)
    }
}
