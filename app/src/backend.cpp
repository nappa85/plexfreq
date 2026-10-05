#include "backend.h"
#include "i18n.h"
#include <QJsonDocument>
#include <QCoreApplication>
#include <QTimer>

QVariant EntryModel::data(const QModelIndex &index,int role) const {
    if(!index.isValid() || index.row()>=m_entries.size())return QVariant();
    if(role==Qt::UserRole+1)return m_entries[index.row()];
    if(role==Qt::UserRole+2)return QCoreApplication::translate("AlbumType",m_entries[index.row()].toMap().value("albumType").toString().toUtf8().constData());
    if(role==Qt::UserRole+3)return m_entries[index.row()].toMap().value("discoveryGroup");
    return QVariant();
}
void EntryModel::replace(const QVariantList &entries) {
    if(entries==m_entries)return;
    // Queue occurrences repeat ratingKey with distinct playQueueItemID; regular
    // playlist rows repeat it with distinct playlistItemId. Identity must
    // include the occurrence or radio repeats collapse into one row.
    // Upper/lower casings are legacy variants of the same field: separate them
    // so "12"+"34" never collides with "123"+"4".
    auto identity=[](const QVariant &value){const auto e=value.toMap();return e.value("type").toString()+":"+e.value("ratingKey").toString()+":"+e.value("key").toString()+":"+e.value("playQueueItemID").toString()+":"+e.value("playQueueItemId").toString()+":"+e.value("playlistItemID").toString()+":"+e.value("playlistItemId").toString();};
    int prefix=0;while(prefix<m_entries.size() && prefix<entries.size() && identity(m_entries[prefix])==identity(entries[prefix]))++prefix;
    int suffix=0;while(suffix<m_entries.size()-prefix && suffix<entries.size()-prefix && identity(m_entries[m_entries.size()-1-suffix])==identity(entries[entries.size()-1-suffix]))++suffix;
    int remove=m_entries.size()-prefix-suffix;if(remove>0){beginRemoveRows(QModelIndex(),prefix,prefix+remove-1);for(int i=0;i<remove;++i)m_entries.removeAt(prefix);endRemoveRows();}
    int add=entries.size()-prefix-suffix;if(add>0){beginInsertRows(QModelIndex(),prefix,prefix+add-1);for(int i=0;i<add;++i)m_entries.insert(prefix+i,entries[prefix+i]);endInsertRows();}
    for(int i=0;i<entries.size();++i)if(m_entries[i]!=entries[i]){m_entries[i]=entries[i];emit dataChanged(index(i),index(i),{Qt::UserRole+1,Qt::UserRole+2,Qt::UserRole+3});}
    emit entriesChanged();
}
void EntryModel::append(const QVariantList &entries){if(entries.isEmpty())return;const int first=m_entries.size();beginInsertRows(QModelIndex(),first,first+entries.size()-1);m_entries.append(entries);endInsertRows();emit entriesChanged();}
static QVariantMap decoded(char *text){if(!text)return {{"accepted",false},{"error","Backend unavailable"}};const auto value=QJsonDocument::fromJson(QByteArray(text)).toVariant().toMap();pf_string_free(text);return value;}
Backend::Backend(const QString &directory,QObject *parent):QObject(parent) {
    m_state={{"items",QVariantList()},{"libraries",QVariantList()},{"servers",QVariantList()},{"queue",QVariantMap{{"items",QVariantList()},{"repeat","off"},{"shuffled",false}}}};
    m_state.insert("networkOnline",false);m_state.insert("networkWifi",false);
    m_cache={{"enabled",true},{"limitMb",512},{"ahead",5},{"tracks",0},{"bytes",0},{"readyKeys",QVariantList()},{"jobs",QVariantList()},{"pinnedKeys",QVariantList()},{"pinnedGroups",QVariantList()},{"paused",false},{"wifiOnly",false},{"waitingForWifi",false},{"error",""}};
    const auto directoryBytes=directory.toUtf8();m_runtime=pf_runtime_new(directoryBytes.constData());
    auto *timer=new QTimer(this);timer->setInterval(100);connect(timer,&QTimer::timeout,this,&Backend::poll);timer->start();
    command("status");
}
Backend::~Backend(){pf_runtime_free(m_runtime);}
void Backend::status(const QVariantMap &value){const bool busy=value.value("busy").toBool(),more=value.value("loadingMore").toBool();if(busy!=m_busy){m_busy=busy;emit busyChanged();}if(more!=m_more){m_more=more;emit loadingMoreChanged();}}
void Backend::command(const QString &op,const QVariantMap &args) {
    auto input=args;input.insert("op",op);const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);const auto accepted=decoded(pf_runtime_submit(m_runtime,bytes.constData()));
    if(accepted.value("accepted").toBool())status(accepted);
    if(accepted.value("clearError").toBool() && !m_error.isEmpty()){m_error.clear();emit errorChanged();}
    if(accepted.value("resetItems").toBool()){m_items.replace(QVariantList());m_state.insert("items",QVariantList());emit stateChanged();}
    if(!accepted.value("accepted").toBool()){m_error=translatedError(accepted);emit errorChanged();}
}
void Backend::poll() {
    const auto value=decoded(pf_runtime_poll(m_runtime));status(value);
    const auto playback=value.value("playback").toMap();if(playback!=m_playback){m_playback=playback;emit playbackChanged();}
    for(const auto &event:value.value("events").toList())handle(event.toMap());
}
void Backend::handle(const QVariantMap &event) {
    if(event.value("kind")=="raise"){emit raiseRequested();return;}if(event.value("kind")=="seeked"){emit seeked(event.value("position").toLongLong());return;}
    const auto input=event.value("request").toMap(),response=event.value("response").toMap();const auto op=input.value("op").toString();const bool ok=response.value("ok").toBool();auto data=response.value("data").toMap();
    if(data.value("_discarded").toBool()){emit completed(op,ok,data);return;}
    if(!ok){if(op!="lyrics" && op!="similar_artists"){m_error=translatedError(response);emit errorChanged();}emit completed(op,false,data);return;}
    translateErrorFields(data);
    bool changed=false;
    if(data.contains("cache")){const auto cache=data.take("cache").toMap();if(cache!=m_cache){m_cache=cache;emit cacheChanged();}}
    if(data.contains("items")){auto items=data.value("items").toList();if(data.value("_listAction")=="append"){m_items.append(items);auto previous=m_state.value("items").toList();previous.append(items);items=previous;data.insert("items",items);}else m_items.replace(items);}
    if(data.contains("queue"))m_queue.replace(data.value("queue").toMap().value("items").toList());
    for(auto i=data.constBegin();i!=data.constEnd();++i){if(i.key().startsWith('_') || op=="lyrics" || op=="similar_artists")continue;if(m_state.value(i.key())!=i.value()){m_state.insert(i.key(),i.value());changed=true;}}
    if(changed)emit stateChanged();
    emit completed(op,true,data);
}
