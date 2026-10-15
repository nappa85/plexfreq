import QtQuick 2.6

// Shared per-item context-menu catalogue for Silica and desktop lists/grids.
// Native menus only render these IDs; visibility/enabled rules live here so
// both frontends offer the same actions (e.g. queue actions only for the item
// types Session.enqueue handles, no Plex edits while offline).
Item {
    id: actions
    visible: false
    property var music
    property bool offline: !!backend.state.offlineMode

    // Full list-row menu, discovery cards and library grid cells.
    property var rowActions: ["waypoint", "editSmart", "similar", "adventureStart", "adventureEnd",
        "downloadRadio", "downloadPlaylist", "playNext", "addQueue", "addPlaylist", "createPlaylist",
        "mixSeed", "renamePlaylist", "deletePlaylist", "playlistUp", "playlistDown", "playlistRemove",
        "favorite", "pin", "queueUp", "queueDown", "queueRemove"]
    property var cardActions: ["playNext", "addQueue", "addPlaylist", "similar", "favorite"]
    property var gridActions: ["playNext", "addQueue", "addPlaylist", "similar", "adventureStart",
        "downloadRadio", "mixSeed", "favorite"]

    function pinned(item) {
        return !!item && backend.cache.pinnedGroups.indexOf(item.type+":"+item.ratingKey)>=0
    }
    function definition(id, item, index) {
        var action={id:id,text:"",visible:false,enabled:true}
        if (!music || !item) return action
        var type=item.type
        var queueable=type==="track" || type==="album"
        var editablePlaylist=!!music.playlist && !music.playlist.smart
        var userPlaylist=type==="playlist" && !item.station
        switch (id) {
        case "waypoint": action.text=qsTr("Add sonic waypoint"); action.visible=type==="track"; action.enabled=music.journeyTracks.length<8; break
        case "editSmart": action.text=qsTr("Edit smart filters"); action.visible=userPlaylist && !!item.smart; action.enabled=!offline; break
        case "similar": action.text=qsTr("Sonically similar"); action.visible=music.canRadio(item); break
        case "adventureStart": action.text=qsTr("Start sonic adventure here"); action.visible=type==="track"; break
        case "adventureEnd": action.text=qsTr("Sonic Adventure to this track"); action.visible=type==="track" && !!music.adventureStart; break
        case "downloadRadio": action.text=qsTr("Download radio · %1 minutes").arg(music.downloadMinutes); action.visible=!!item.station || music.canRadio(item); action.enabled=!offline; break
        case "downloadPlaylist": action.text=qsTr("Download %1 minutes").arg(music.downloadMinutes); action.visible=userPlaylist; action.enabled=!offline; break
        case "playNext": action.text=qsTr("Play next"); action.visible=!music.showQueue && queueable; break
        case "addQueue": action.text=qsTr("Add to queue"); action.visible=!music.showQueue && queueable; break
        case "addPlaylist": action.text=qsTr("Add to playlist"); action.visible=queueable; action.enabled=!offline; break
        case "createPlaylist": action.text=qsTr("Create playlist from this"); action.visible=queueable; action.enabled=!offline; break
        case "mixSeed": action.text=qsTr("Add as mix seed"); action.visible=type==="artist" || type==="album"; break
        case "renamePlaylist": action.text=qsTr("Rename playlist"); action.visible=userPlaylist; action.enabled=!offline; break
        case "deletePlaylist": action.text=qsTr("Delete playlist"); action.visible=userPlaylist; action.enabled=!offline; break
        case "playlistUp": action.text=qsTr("Move playlist entry up"); action.visible=editablePlaylist; action.enabled=!offline && index>0; break
        case "playlistDown": action.text=qsTr("Move playlist entry down"); action.visible=editablePlaylist; action.enabled=!offline && index+1<music.items.length; break
        case "playlistRemove": action.text=qsTr("Remove from playlist"); action.visible=editablePlaylist; action.enabled=!offline; break
        case "favorite": action.text=item.userRating>=10 ? qsTr("Remove favorite") : qsTr("Add to favorites"); action.visible=type==="track"; break
        case "pin": action.text=pinned(item) ? qsTr("Unpin download") : qsTr("Pin for offline listening"); action.visible=!item.station && (queueable || type==="playlist"); break
        case "queueUp": action.text=qsTr("Move up"); action.visible=music.showQueue; action.enabled=index>0; break
        case "queueDown": action.text=qsTr("Move down"); action.visible=music.showQueue; action.enabled=index+1<backend.state.queue.items.length; break
        case "queueRemove": action.text=qsTr("Remove from queue"); action.visible=music.showQueue; break
        default: console.warn("unknown item action: " + id)
        }
        return action
    }
    function activate(id, item, index) {
        var action=definition(id, item, index)
        if (!action.visible || !action.enabled) return
        switch (id) {
        case "waypoint": music.addJourneyTrack(item); break
        case "editSmart": music.editFilters("update",item); break
        case "similar": music.sonicNeighbors(item); break
        case "adventureStart": music.adventureStart=item; break
        case "adventureEnd": music.sonicAdventure(item); break
        case "downloadRadio": music.downloadRadio(item); break
        case "downloadPlaylist": backend.command("download_plan",{kind:"playlist",key:item.ratingKey,minutes:music.downloadMinutes}); break
        case "playNext": music.enqueue(item,true); break
        case "addQueue": music.enqueue(item,false); break
        case "addPlaylist": music.playlistEditor("add",item); break
        case "createPlaylist": music.playlistEditor("create",item); break
        case "mixSeed": music.addMixSeed(item); break
        case "renamePlaylist": music.playlistEditor("rename",item); break
        case "deletePlaylist": backend.command("playlist_edit",{action:"delete",key:item.ratingKey}); break
        case "playlistUp": music.playlistMove(index,false); break
        case "playlistDown": music.playlistMove(index,true); break
        case "playlistRemove": backend.command("playlist_edit",{action:"remove",key:music.playlist.ratingKey,item_id:item.playlistItemId}); break
        case "favorite": music.favorite(item); break
        case "pin": backend.command("pin",{key:item.ratingKey,kind:item.type,enabled:!pinned(item)}); break
        case "queueUp": music.moveQueue(index,index-1); break
        case "queueDown": music.moveQueue(index,index+1); break
        case "queueRemove": backend.command("queue_remove",{index:index}); break
        }
    }
}
