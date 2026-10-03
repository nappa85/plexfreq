import QtQuick 2.6
import "../qml/shared"

Item {
    width: 480; height: 1000
    Column {
        width: parent.width
        Rectangle { width: parent.width; height: 80 }
        IntrinsicLoader {
            id: loader; objectName: "headerLoader"; width: parent.width
            sourceComponent: Component {
                Column {
                    objectName: "detailContent"
                    Rectangle { width: parent.width; height: 200 }
                    Text {
                        width: parent.width; wrapMode: Text.Wrap; font.pixelSize: 20
                        text: new Array(21).join("Fixture biography with enough content to wrap across many lines. ")
                    }
                    Rectangle { width: parent.width; height: 80 }
                }
            }
        }
        Rectangle { objectName: "firstRow"; width: parent.width; height: 80 }
    }
}
