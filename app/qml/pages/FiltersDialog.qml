import QtQuick 2.6
import Sailfish.Silica 1.0

Dialog {
    property var music
    canAccept:!backend.busy && !music.filterLoading && music.filterEditable && (music.filterAction==="browse" || name.text.trim().length>0) && (!from.text.length || from.acceptableInput) && (!to.text.length || to.acceptableInput) && count.acceptableInput
    onAccepted:music.submitFilters()
    SilicaFlickable {anchors.fill:parent;contentHeight:body.height
        Column {id:body;width:parent.width;spacing:Theme.paddingSmall
            DialogHeader {title:music.filterAction==="browse" ? qsTr("Library filters") : qsTr("Smart playlist")}
            BusyIndicator {anchors.horizontalCenter:parent.horizontalCenter;running:music.filterLoading;visible:running}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;visible:!music.filterEditable;text:qsTr("These smart-playlist rules cannot be represented by this editor.");wrapMode:Text.Wrap;color:Theme.secondaryColor}
            TextField {id:name;width:parent.width;visible:music.filterAction!=="browse";label:qsTr("Playlist name");text:music.filterName;onTextChanged:music.filterName=text}
            ComboBox {width:parent.width;label:qsTr("Browse");visible:music.filterAction==="browse";value:music.filterKind==="artist" ? qsTr("Artists") : music.filterKind==="album" ? qsTr("Albums") : qsTr("Tracks")
                menu:ContextMenu {Repeater {model:["artist","album","track"];MenuItem {text:modelData==="artist" ? qsTr("Artists") : modelData==="album" ? qsTr("Albums") : qsTr("Tracks");onClicked:{music.filterKind=modelData;music.requestFilterChoices()}}}}
            }
            Repeater {model:[{field:"genre",label:qsTr("Genre")},{field:"mood",label:qsTr("Mood")},{field:"style",label:qsTr("Style")}]
                ComboBox {id:picker;property string field:modelData.field;property var choices:[{key:"",title:qsTr("Any")}].concat(music.filterChoices[field] || []);width:body.width;label:modelData.label;currentIndex:music.filterChoiceIndex(choices,music.filterValues[field]);enabled:choices.length>1
                    menu:ContextMenu {Repeater {model:picker.choices;MenuItem {text:modelData.title;onClicked:music.filterValue(picker.field,modelData.key)}}}
                }
            }
            TextField {width:parent.width;label:qsTr("Title contains");text:music.filterText("title");onTextChanged:music.filterValue("title",text)}
            TextField {id:from;width:parent.width;enabled:music.filterKind!=="artist";label:qsTr("Year from");text:music.filterText("yearFrom");inputMethodHints:Qt.ImhDigitsOnly;validator:IntValidator {bottom:1;top:9999}
                onTextChanged:music.filterValue("yearFrom",text.length ? Number(text) : null)}
            TextField {id:to;width:parent.width;enabled:music.filterKind!=="artist";label:qsTr("Year to");text:music.filterText("yearTo");inputMethodHints:Qt.ImhDigitsOnly;validator:IntValidator {bottom:1;top:9999}
                onTextChanged:music.filterValue("yearTo",text.length ? Number(text) : null)}
            ComboBox {width:parent.width;label:qsTr("Minimum rating");value:music.filterValues.minRating===null ? qsTr("Any") : music.filterValues.minRating+"/10"
                menu:ContextMenu {Repeater {model:[0,5,8,10];MenuItem {text:modelData ? modelData+"/10" : qsTr("Any");onClicked:music.filterValue("minRating",modelData || null)}}}
            }
            TextSwitch {text:qsTr("Unplayed only");checked:!!music.filterValues.unplayed;onClicked:music.filterValue("unplayed",checked)}
            ComboBox {width:parent.width;label:qsTr("Sort");value:music.filterSortText(music.filterSort)
                menu:ContextMenu {Repeater {model:["title","newest","year","played","popular","random"];MenuItem {text:music.filterSortText(modelData);onClicked:music.filterSort=modelData}}}
            }
            TextField {id:count;width:parent.width;visible:music.filterAction!=="browse";label:qsTr("Maximum tracks");text:""+music.filterLimit;inputMethodHints:Qt.ImhDigitsOnly;validator:IntValidator {bottom:1;top:1000}
                onTextChanged:music.filterLimit=Number(text)}
        }
        VerticalScrollDecorator {}
    }
}
