import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    id:dialog
    property var music
    title:music.filterAction==="browse" ? qsTr("Library filters") : qsTr("Smart playlist")
    standardButtons:Dialog.Save|Dialog.Cancel
    function validate() {if(!name || !from || !to || !count)return;var save=standardButton(Dialog.Save);if(save)save.enabled=!backend.busy && !music.filterLoading && music.filterEditable && (music.filterAction==="browse" || name.text.trim().length>0) && (!from.text.length || from.acceptableInput) && (!to.text.length || to.acceptableInput) && count.acceptableInput}
    onOpened:validate()
    onAccepted:music.submitFilters()
    Connections {target:backend;onBusyChanged:dialog.validate()}
    Connections {target:music;onFilterEditableChanged:dialog.validate();onFilterLoadingChanged:dialog.validate()}
    contentItem:ScrollView {implicitWidth:440;implicitHeight:500;clip:true
        ColumnLayout {width:parent.width
            BusyIndicator {running:music.filterLoading;visible:running}
            Label {Layout.fillWidth:true;visible:!music.filterEditable;text:qsTr("These smart-playlist rules cannot be represented by this editor.");wrapMode:Text.Wrap}
            TextField {id:name;Layout.fillWidth:true;visible:music.filterAction!=="browse";placeholderText:qsTr("Playlist name");text:music.filterName;onTextChanged:{music.filterName=text;dialog.validate()}}
            ComboBox {Layout.fillWidth:true;visible:music.filterAction==="browse";model:[qsTr("Artists"),qsTr("Albums"),qsTr("Tracks")];currentIndex:music.filterKind==="artist" ? 0 : music.filterKind==="album" ? 1 : 2;onActivated:{music.filterKind=["artist","album","track"][currentIndex];music.requestFilterChoices()}}
            Repeater {model:[{field:"genre",label:qsTr("Genre")},{field:"mood",label:qsTr("Mood")},{field:"style",label:qsTr("Style")}]
                ComboBox {property string field:modelData.field;property string caption:modelData.label;Layout.fillWidth:true;model:[{key:"",title:qsTr("Any")}].concat(music.filterChoices[field] || []);textRole:"title";currentIndex:music.filterChoiceIndex(model,music.filterValues[field]);displayText:caption+": "+currentText;enabled:model.length>1;onActivated:music.filterValue(field,model[currentIndex].key)}
            }
            TextField {Layout.fillWidth:true;placeholderText:qsTr("Title contains");text:music.filterText("title");onTextChanged:music.filterValue("title",text)}
            TextField {id:from;Layout.fillWidth:true;enabled:music.filterKind!=="artist";placeholderText:qsTr("Year from");text:music.filterText("yearFrom");validator:IntValidator {bottom:1;top:9999}
                onTextChanged:{music.filterValue("yearFrom",text.length ? Number(text) : null);dialog.validate()}}
            TextField {id:to;Layout.fillWidth:true;enabled:music.filterKind!=="artist";placeholderText:qsTr("Year to");text:music.filterText("yearTo");validator:IntValidator {bottom:1;top:9999}
                onTextChanged:{music.filterValue("yearTo",text.length ? Number(text) : null);dialog.validate()}}
            ComboBox {Layout.fillWidth:true;textRole:"text";valueRole:"value"
                model:[0,5,8,10].map(function(rating){return {value:rating,text:rating ? rating+"/10" : qsTr("Any")}})
                currentIndex:count>0 ? indexOfValue(music.filterValues.minRating || 0) : -1;displayText:qsTr("Minimum rating")+": "+(music.filterValues.minRating===null ? qsTr("Any") : music.filterValues.minRating+"/10");onActivated:music.filterValue("minRating",currentValue || null)}
            Switch {text:qsTr("Unplayed only");checked:!!music.filterValues.unplayed;onClicked:music.filterValue("unplayed",checked)}
            ComboBox {Layout.fillWidth:true;textRole:"text";valueRole:"value"
                model:["title","newest","year","played","popular","random"].map(function(sort){return {value:sort,text:music.filterSortText(sort)}})
                currentIndex:count>0 ? indexOfValue(music.filterSort) : -1;displayText:qsTr("Sort")+": "+music.filterSortText(music.filterSort);onActivated:music.filterSort=currentValue}
            TextField {id:count;Layout.fillWidth:true;visible:music.filterAction!=="browse";placeholderText:qsTr("Maximum tracks");text:""+music.filterLimit;validator:IntValidator {bottom:1;top:1000}
                onTextChanged:{if(acceptableInput)music.filterLimit=Number(text);dialog.validate()}}
        }
    }
}
