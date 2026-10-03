#include "backend.h"
#include "mpris.h"
#include "plexfreq_core.h"
#include <QJsonDocument>
#include <QJsonObject>
#include <QMutexLocker>
#include <QUrl>
#include <QTimer>
#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusArgument>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusVariant>
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
#include <QAudioOutput>
#else
#include <QMediaContent>
#endif

void EntryModel::replace(const QVariantList &entries) {
    if (entries == m_entries) return;
    auto identity = [](const QVariant &entry) { const auto item=entry.toMap(); return item.value("type").toString()+":"+item.value("ratingKey").toString()+":"+item.value("key").toString(); };
    int prefix=0; while (prefix<m_entries.size() && prefix<entries.size() && identity(m_entries[prefix])==identity(entries[prefix])) ++prefix;
    int suffix=0; while (suffix<m_entries.size()-prefix && suffix<entries.size()-prefix && identity(m_entries[m_entries.size()-1-suffix])==identity(entries[entries.size()-1-suffix])) ++suffix;
    const int removed=m_entries.size()-prefix-suffix;
    if (removed>0) { beginRemoveRows(QModelIndex(),prefix,prefix+removed-1); for (int i=0;i<removed;++i) m_entries.removeAt(prefix); endRemoveRows(); }
    const int added=entries.size()-prefix-suffix;
    if (added>0) { beginInsertRows(QModelIndex(),prefix,prefix+added-1); for (int i=0;i<added;++i) m_entries.insert(prefix+i,entries[prefix+i]); endInsertRows(); }
    for (int i=0;i<entries.size();++i) if (m_entries[i]!=entries[i]) { m_entries[i]=entries[i]; emit dataChanged(index(i),index(i),{Qt::UserRole+1}); }
}
void EntryModel::append(const QVariantList &entries) {
    if (entries.isEmpty()) return;
    const int first = m_entries.size();
    beginInsertRows(QModelIndex(), first, first + entries.size() - 1);
    m_entries.append(entries); endInsertRows();
}
static bool isListing(const QString &op) { return op=="playlist_items" || op=="library_browse" || op=="artist_albums" || op == "search" || op == "collection" || op == "offline_browse" || op == "browse" || op == "detail" || op == "children" || op == "playlists" || op == "cached_tracks" || op == "jump_artist"; }
static bool isPage(const QString &op, const QVariantMap &args) { return isListing(op) && args.value("start").toInt() > 0 && !args.value("_replace").toBool(); }
static bool isPassive(const QString &op, const QVariantMap &args) { return op=="network_state" || op=="playback_event" || op=="sync_history" || op=="playlist_choices" || op == "cache_artwork" || op == "lyrics" || op == "save_playback" || op == "cache_status" || op == "timeline" || op == "alphabet" || op == "similar_artists" || args.value("_background").toBool() || isPage(op,args); }

CoreThread::CoreThread(const QString &directory, QObject *parent)
    : QThread(parent), m_directory(directory) {}
CoreThread::~CoreThread() {
    { QMutexLocker lock(&m_mutex); m_stopping = true; m_wake.wakeOne(); }
    wait(); // Network operations have bounded timeouts; never destroy a live Rust handle.
}
void CoreThread::submit(const QByteArray &request) {
    QMutexLocker lock(&m_mutex);
    m_requests.enqueue(request);
    m_wake.wakeOne();
}
void CoreThread::networkHint(bool wifi) {QMutexLocker lock(&m_mutex);m_networkHintKnown=true;m_wifiHint=wifi;if(m_downloadControl)pf_download_network(m_downloadControl,wifi);}
void CoreThread::policyHint(bool wifiOnly,bool paused) {QMutexLocker lock(&m_mutex);m_policyHintKnown=true;m_wifiPolicyHint=wifiOnly;m_pausePolicyHint=paused;if(m_downloadControl)pf_download_policy(m_downloadControl,wifiOnly,paused);}
void CoreThread::run() {
    Core *core = pf_new(m_directory.toUtf8().constData());
    auto *control=pf_download_control(core);{QMutexLocker lock(&m_mutex);m_downloadControl=control;if(m_policyHintKnown)pf_download_policy(control,m_wifiPolicyHint,m_pausePolicyHint);if(m_networkHintKnown)pf_download_network(control,m_wifiHint);}
    for (;;) {
        QByteArray request;
        {
            QMutexLocker lock(&m_mutex);
            while (m_requests.isEmpty() && !m_stopping) m_wake.wait(&m_mutex);
            if (m_stopping && m_requests.isEmpty()) break;
            request = m_requests.dequeue();
        }
        char *response = pf_call(core, request.constData());
        const QByteArray copy(response);
        pf_string_free(response);
        emit result(request, copy);
    }
    {QMutexLocker lock(&m_mutex);m_downloadControl=nullptr;}
    pf_free(core);pf_download_control_free(control);
}

Backend::Backend(const QString &directory, QObject *parent) : QObject(parent),
    m_thread(new CoreThread(directory, this)), m_player(new QMediaPlayer(this)) {
    m_state.insert("items", QVariantList());
    m_state.insert("libraries", QVariantList());
    m_state.insert("servers", QVariantList());
    m_state.insert("radio", QVariant());
    m_state.insert("detail", QVariantMap());
    m_state.insert("networkOnline",false);m_state.insert("networkWifi",false);
    m_state.insert("queue", QVariantMap{{"items", QVariantList()}, {"repeat", "off"}, {"shuffled", false}});
    m_cache = {{"enabled",true},{"limitMb",512},{"ahead",5},{"bytes",0},{"tracks",0},{"readyKeys",QVariantList()},{"downloading",false},{"pending",0},{"error",""}};
    m_cache.insert("pinnedKeys",QVariantList());m_cache.insert("pinnedGroups",QVariantList());
    m_cache.insert("jobs",QVariantList());m_cache.insert("wifiOnly",false);m_cache.insert("paused",false);m_cache.insert("waitingForWifi",false);
    connect(m_thread, &CoreThread::result, this, &Backend::handle);
    connect(m_player, &QMediaPlayer::positionChanged, this, &Backend::playbackChanged);
    connect(m_player, &QMediaPlayer::durationChanged, this, &Backend::playbackChanged);
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    auto *audio = new QAudioOutput(m_player);
    m_player->setAudioOutput(audio);
    audio->setVolume(0.8);
    connect(m_player, &QMediaPlayer::playbackStateChanged, this, &Backend::playbackChanged);
    connect(m_player, &QMediaPlayer::playbackStateChanged, this, [this] { reportTimeline(); });
    connect(m_player, &QMediaPlayer::errorOccurred, this, [this](QMediaPlayer::Error, const QString &) {
#else
    m_player->setAudioRole(QAudio::MusicRole);
    m_player->setVolume(80);
    connect(m_player, &QMediaPlayer::stateChanged, this, &Backend::playbackChanged);
    connect(m_player, &QMediaPlayer::stateChanged, this, [this] { reportTimeline(); });
    connect(m_player, static_cast<void (QMediaPlayer::*)(QMediaPlayer::Error)>(&QMediaPlayer::error), this, [this](QMediaPlayer::Error) {
#endif
        // Multimedia errors can include credential-bearing URLs. Keep UI diagnostics generic.
        if (!m_source.isLocalFile() && !m_state.value("track").toMap().isEmpty() && !busy()) {
            m_restorePosition = position();
            command("cached_playback");
            return;
        }
        m_error = tr("Audio playback failed. This track may not be downloaded yet, or its codec is unsupported.");
        emit errorChanged();
    });
    connect(m_player, &QMediaPlayer::mediaStatusChanged, this, [this](QMediaPlayer::MediaStatus status) {
        if (m_restorePosition >= 0 && (status == QMediaPlayer::LoadedMedia || status == QMediaPlayer::BufferedMedia) && m_player->isSeekable()) {
            const qint64 restore = m_restorePosition; m_restorePosition = -1; m_player->setPosition(restore);
        }
        if (status == QMediaPlayer::EndOfMedia) {
            recordPlayback();sendTimeline("stopped",true);
            command("next", {{"automatic", true}});
        }
    });
    auto *timelineTimer = new QTimer(this);
    timelineTimer->setInterval(10000);
    connect(timelineTimer, &QTimer::timeout, this, &Backend::reportTimeline);
    timelineTimer->start();
    auto *cacheTimer = new QTimer(this);
    cacheTimer->setInterval(2000);
    connect(cacheTimer, &QTimer::timeout, this, [this] {
        if (m_pending == 0 && (m_cache.value("enabled").toBool() || m_cache.value("pending").toInt()>0 || m_cache.value("tracks").toInt()>0)) command("cache_status");
    });
    cacheTimer->start();
    auto *saveTimer=new QTimer(this); saveTimer->setInterval(5000);
    connect(saveTimer,&QTimer::timeout,this,[this]{if(m_pending==0)savePlayback();}); saveTimer->start();
    m_sampleClock.start();auto *sampleTimer=new QTimer(this);sampleTimer->setInterval(1000);connect(sampleTimer,&QTimer::timeout,this,&Backend::samplePlayback);sampleTimer->start();
    auto *networkTimer=new QTimer(this);networkTimer->setInterval(5000);connect(networkTimer,&QTimer::timeout,this,&Backend::updateNetwork);networkTimer->start();
    auto *historyTimer=new QTimer(this);historyTimer->setInterval(30000);connect(historyTimer,&QTimer::timeout,this,[this]{if(m_pending==0 && m_state.value("networkOnline").toBool() && !m_state.value("offlineMode").toBool())command("sync_history");});historyTimer->start();
    auto systemBus=QDBusConnection::systemBus();
    systemBus.connect("net.connman","/","net.connman.Manager","ServicesChanged",this,SLOT(updateNetwork()));
    systemBus.connect("net.connman",QString(),"net.connman.Service","PropertyChanged",this,SLOT(updateNetwork()));
    m_thread->start();
    new MprisService(this);
    connect(QCoreApplication::instance(),&QCoreApplication::aboutToQuit,this,&Backend::savePlayback);
    command("status");
    updateNetwork();
}
bool Backend::playing() const {
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    return m_player->playbackState() == QMediaPlayer::PlayingState;
#else
    return m_player->state() == QMediaPlayer::PlayingState;
#endif
}
bool Backend::paused() const {
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    return m_player->playbackState()==QMediaPlayer::PausedState;
#else
    return m_player->state()==QMediaPlayer::PausedState;
#endif
}
double Backend::volume() const {
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    return m_player->audioOutput()->volume();
#else
    return m_player->volume()/100.;
#endif
}
void Backend::command(const QString &op, const QVariantMap &args) {
    if(op=="download_policy")m_thread->policyHint(args.value("wifi_only").toBool(),args.value("paused").toBool());
    if(!m_switchingSource && (op=="play" || op=="play_album" || op=="select_track" || op=="next" || op=="previous" || op=="radio" || op=="mix" || op=="stop_radio" || op=="logout" || op=="connect" || op=="select_server")) {recordPlayback();sendTimeline("stopped");}
    QVariantMap request(args);
    request.insert("op", op);
    if (isListing(op)) {
        if (!isPage(op,args) && !args.value("_refresh").toBool()) {
            ++m_viewGeneration;
            m_itemsModel.replace(QVariantList());
            if (!m_state.value("items").toList().isEmpty()) { m_state.insert("items", QVariantList()); emit stateChanged(); }
        }
        request.insert("_view", m_viewGeneration);
    }
    if (op == "similar_artists") request.insert("_view", m_viewGeneration);
    m_pending++;
    if (isPage(op,args)) { if (m_morePending++ == 0) emit loadingMoreChanged(); }
    if (!isPassive(op,args)) {
        if (m_busyPending++ == 0) emit busyChanged();
        if (!m_error.isEmpty()) { m_error.clear(); emit errorChanged(); }
    }
    m_thread->submit(QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact));
}
void Backend::handle(const QByteArray &request, const QByteArray &response) {
    const auto envelope = QJsonDocument::fromJson(response).object();
    const auto input = QJsonDocument::fromJson(request).object();
    const QString op = input.value("op").toString();
    const auto args = input.toVariantMap();
    const bool ok = envelope.value("ok").toBool();
    QVariantMap data = envelope.value("data").toObject().toVariantMap();
    const bool discarded = (isListing(op) || op == "similar_artists") && input.value("_view").toVariant().toULongLong() != m_viewGeneration;
    bool changed = false;
    if (!discarded && ok) {
        if(data.contains("stream"))m_switchingSource=true;
        if(op=="status")m_listened=data.value("resumeListened").toLongLong();
        const auto local=data.take("localArtwork").toString();
        if (!local.isEmpty()) {auto track=m_state.value("track").toMap();if(track.value("artwork").toString()!=local){track.insert("artwork",local);m_state.insert("track",track);changed=true;}}
        const auto albumArt=data.take("localAlbumArtwork").toString();
        if (!albumArt.isEmpty()) {auto track=m_state.value("track").toMap();if(track.value("albumArtwork").toString()!=albumArt){track.insert("albumArtwork",albumArt);m_state.insert("track",track);changed=true;}}
        if (data.contains("ratedKey")) {
            auto items=m_state.value("items").toList();
            for(auto &value:items){auto item=value.toMap();if(item.value("ratingKey")==data.value("ratedKey")){item.insert("userRating",data.value("userRating"));value=item;}}
            m_state.insert("items",items);m_itemsModel.replace(items);changed=true;
        }
        if (data.contains("cache")) { const auto cache = data.take("cache").toMap(); if (cache != m_cache) { m_cache = cache; emit cacheChanged(); } }
        if (data.contains("items") && isPage(op,args) && !data.value("replaceItems").toBool()) {
            m_itemsModel.append(data.value("items").toList());
            QVariantList items = m_state.value("items").toList();
            items.append(data.value("items").toList());
            data.insert("items", items);
        } else if (data.contains("items")) {
            m_itemsModel.replace(data.value("items").toList());
        }
        if (op == "connect" || op == "select_server" || op == "logout") {
            m_player->stop();
            m_source=QUrl();m_restorePosition=-1;
            m_state.insert("track", QVariantMap());
            m_state.insert("items", QVariantList());
            m_state.insert("detail", QVariantMap());
            m_state.insert("playlistChoices",QVariantList());
            m_state.insert("hasMore", false);
            m_state.insert("queue", QVariantMap{{"items", QVariantList()}, {"repeat", "off"}, {"shuffled", false}});
            m_itemsModel.replace(QVariantList()); m_queueModel.replace(QVariantList()); changed = true;
            if (op == "logout") { m_state.insert("servers", QVariantList()); m_state.insert("libraries", QVariantList()); }
        }
        for (auto i = data.constBegin(); i != data.constEnd(); ++i) {
            if (op != "similar_artists" && op != "lyrics" && i.key() != "stream" && i.key() != "url" && m_state.value(i.key()) != i.value()) { m_state.insert(i.key(), i.value()); changed = true; }
        }
        if (data.contains("queue")) m_queueModel.replace(data.value("queue").toMap().value("items").toList());
        if(op!="cache_artwork" && op!="cache_status" && op!="status" && (data.contains("items") || data.contains("detail") || data.contains("stream"))) {
            QVariantList artItems=data.value("items").toList();
            if(data.contains("detail"))artItems.prepend(data.value("detail"));
            if(data.contains("track"))artItems.prepend(data.value("track"));
            if(!artItems.isEmpty())command("cache_artwork",{{"items",artItems}});
        }
        if (data.contains("stream")) {
            const QUrl url(data.value("stream").toString());
            if (op != "cached_playback") m_restorePosition = data.contains("resumePosition") ? data.value("resumePosition").toLongLong() : -1;
            m_source = url;
            m_player->stop();
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
            m_player->setSource(url);
#else
            m_player->setMedia(QMediaContent(url));
#endif
            m_listened=data.value("resumeListened").toLongLong();m_samplePosition=0;m_sampleClock.restart();m_wasPlaying=false;m_stoppedReported=false;m_switchingSource=false;
            if (!url.isEmpty()) {m_player->play();if(args.value("_paused").toBool())m_player->pause();}
        }
        if (changed) emit stateChanged();
    } else if (!discarded && !ok && op != "similar_artists" && op != "lyrics") {
        m_error = envelope.value("error").toString(tr("Backend returned an invalid response"));
        emit errorChanged();
    }
    m_pending--;
    if (isPage(op,args) && --m_morePending == 0) emit loadingMoreChanged();
    if (!isPassive(op,args) && --m_busyPending == 0) emit busyChanged();
    if (discarded) { data.clear(); data.insert("_discarded",true); }
    else if (op == "similar_artists") data.insert("similarKey", args.value("key"));
    else if (op == "lyrics") data.insert("lyricsKey",args.value("key"));
    emit completed(op, ok, data);
}
void Backend::togglePlayback() {
    if (playing()) pause(); else play();
}
void Backend::play() {
    if (m_source.isEmpty()) {
        if (!busy() && !m_state.value("queue").toMap().value("items").toList().isEmpty()) {
            if (m_state.value("track").toMap().isEmpty()) command("select_track",{{"index",0}}); else command("resume");
        }
    } else m_player->play();
}
void Backend::pause() {samplePlayback();m_player->pause();recordPlayback();savePlayback();}
void Backend::stop() {samplePlayback();recordPlayback();m_player->stop();savePlayback();}
void Backend::savePlayback() {
    if (!m_source.isEmpty()) command("save_playback",{{"key",m_state.value("track").toMap().value("ratingKey")},{"position",position()},{"generation",m_state.value("playbackGeneration")}});
    recordPlayback();
}
void Backend::seek(qint64 value) {
    if (m_player->isSeekable()) {samplePlayback();m_player->setPosition(qBound<qint64>(0, value, duration()));m_samplePosition=position();emit seeked(position());savePlayback();}
}
void Backend::setVolume(double value) {
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    m_player->audioOutput()->setVolume(qBound(0.0, value, 1.0));
#else
    m_player->setVolume(qRound(qBound(0.0, value, 1.0) * 100));
#endif
    emit playbackChanged();
}

void Backend::reportTimeline() {
    if(m_switchingSource)return;
    samplePlayback();if(m_pending>0)return;recordPlayback();
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
    const bool paused = m_player->playbackState() == QMediaPlayer::PausedState;
#else
    const bool paused = m_player->state() == QMediaPlayer::PausedState;
#endif
    if (playing() || paused)sendTimeline(playing()?"playing":"paused");
}

void Backend::samplePlayback() {
    const auto elapsed=m_sampleClock.restart();const auto current=position();
    if(!m_switchingSource && m_restorePosition<0 && m_wasPlaying && current>m_samplePosition)m_listened+=qMin<qint64>(current-m_samplePosition,qMin<qint64>(elapsed,5000));
    m_samplePosition=current;m_wasPlaying=playing();
}
void Backend::recordPlayback() {
    if(m_switchingSource || m_source.isEmpty() || duration()<=0)return;
    samplePlayback();command("playback_event",{{"key",m_state.value("track").toMap().value("ratingKey")},{"occurrence",m_state.value("playbackOccurrence")},{"listened",m_listened},{"duration",duration()}});
}
void Backend::sendTimeline(const QString &state,bool continuing) {
    if(m_switchingSource || m_source.isEmpty() || m_source.isLocalFile() || m_state.value("offlineMode").toBool() || duration()<=0)return;
    if(state=="stopped" && m_stoppedReported)return;
    m_stoppedReported=state=="stopped";
    command("timeline",{{"state",state},{"position",position()},{"duration",duration()},{"continuing",continuing}});
}
void Backend::updateNetwork() {
    if(m_networkPending)return;
    m_networkPending=true;
    auto message=QDBusMessage::createMethodCall("net.connman","/","net.connman.Manager","GetServices");
    auto *watcher=new QDBusPendingCallWatcher(QDBusConnection::systemBus().asyncCall(message,2000),this);
    connect(watcher,&QDBusPendingCallWatcher::finished,this,[this](QDBusPendingCallWatcher *reply){
        if(!reply->isError() && !reply->reply().arguments().isEmpty()) {
            const auto argument=reply->reply().arguments().first().value<QDBusArgument>();
            bool online=false,wifi=false;argument.beginArray();
            while(!argument.atEnd()){QDBusObjectPath path;QVariantMap properties;argument.beginStructure();argument>>path>>properties;argument.endStructure();
                const auto state=properties.value("State").toString();if(!online && (state=="ready" || state=="online")){online=true;wifi=properties.value("Type").toString()=="wifi";}}
            argument.endArray();m_networkPending=false;m_thread->networkHint(wifi);command("network_state",{{"wifi",wifi},{"online",online},{"live_hint",true}});reply->deleteLater();return;
        }
        reply->deleteLater();
        auto request=QDBusMessage::createMethodCall("org.freedesktop.NetworkManager","/org/freedesktop/NetworkManager","org.freedesktop.DBus.Properties","GetAll");request<<QString("org.freedesktop.NetworkManager");
        auto *fallback=new QDBusPendingCallWatcher(QDBusConnection::systemBus().asyncCall(request,2000),this);
        connect(fallback,&QDBusPendingCallWatcher::finished,this,[this](QDBusPendingCallWatcher *result){
            QVariantMap values;if(!result->isError() && !result->reply().arguments().isEmpty())values=qdbus_cast<QVariantMap>(result->reply().arguments().first());
            const bool wifi=values.value("PrimaryConnectionType").toString()=="802-11-wireless";
            m_networkPending=false;m_thread->networkHint(wifi);command("network_state",{{"wifi",wifi},{"online",values.value("State").toUInt()>=50},{"live_hint",true}});result->deleteLater();
        });
    });
}
