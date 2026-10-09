#include "backend.h"
#include "i18n.h"
#include <QTranslator>
#include <QtTest>
#include <QTcpServer>
#include <QTcpSocket>
#include <QTemporaryDir>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonArray>
#include <QDataStream>
#include <QRegularExpression>
#include <QFileInfo>
#include <QFile>
#include <QProcess>
#include "plexfreq_core.h"
#include <memory>
#include <QQmlEngine>
#include <QQmlContext>
#include <QQmlComponent>
#include <QQmlExpression>
#include <QQuickItem>
#include <QQuickWindow>
#include <QQuickView>
#if QT_VERSION < QT_VERSION_CHECK(6, 0, 0)
// SDK-only fixture: Qt5.6 has no public positioner forceLayout API. Polish
// the existing window directly rather than creating another GL scenegraph.
#include <QtQuick/private/qquickwindow_p.h>
#endif
#include <QQuickItemGrabResult>
#include <QDBusConnection>
#include <QDBusInterface>
#include <QDBusPendingCallWatcher>
#include <QDBusArgument>
#include <QDBusVariant>

class NavigationBackend : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool busy READ busy NOTIFY busyChanged)
    Q_PROPERTY(bool loadingMore READ loadingMore NOTIFY loadingMoreChanged)
    Q_PROPERTY(QVariantMap state READ state NOTIFY stateChanged)
    Q_PROPERTY(QVariantMap cache READ cache NOTIFY cacheChanged)
    Q_PROPERTY(QAbstractItemModel *itemsModel READ itemsModel CONSTANT)
    Q_PROPERTY(QAbstractItemModel *queueModel READ queueModel CONSTANT)
    Q_PROPERTY(qint64 position READ position NOTIFY playbackChanged)
    Q_PROPERTY(qint64 duration READ duration CONSTANT)
    Q_PROPERTY(bool playing READ playing CONSTANT)
    Q_PROPERTY(QString error READ error CONSTANT)
public:
    QString operation;
    int commandCount=0;
    QVariantMap arguments;
    QVariantMap data{{"detail", QVariantMap()}, {"items", QVariantList()}, {"servers", QVariantList()}, {"queue", QVariantMap{{"items", QVariantList()}}}};
    bool busyValue=false;
    QVariantMap cacheData{{"enabled",true},{"tracks",0},{"readyKeys",QVariantList()},{"pinnedGroups",QVariantList()},{"jobs",QVariantList()},{"bytes",0},{"wifiOnly",false},{"paused",false},{"waitingForWifi",false},{"error",""}};
    EntryModel itemModel;
    EntryModel queuedModel;
    bool busy() const { return busyValue; }
    bool loadingMore() const { return false; }
    qint64 playbackPosition=2000;
    qint64 position() const {return playbackPosition;}
    qint64 duration() const {return 10000;}
    bool playing() const {return false;}
    QString error() const {return QString();}
    QVariantMap state() const { return data; }
    QVariantMap cache() const { return cacheData; }
    QAbstractItemModel *itemsModel() {return &itemModel;}
    QAbstractItemModel *queueModel() {return &queuedModel;}
    Q_INVOKABLE void command(const QString &op, const QVariantMap &args = QVariantMap()) { ++commandCount; operation = op; arguments = args; }
signals:
    void completed(const QString &op, bool ok, const QVariantMap &data);
    void stateChanged();
    void cacheChanged();
    void busyChanged();
    void loadingMoreChanged();
    void playbackChanged();
};

static QQuickItem *visualItem(QQuickItem *parent,const QString &name) {
    if(!parent)return nullptr;
    if(parent->objectName()==name)return parent;
    for(auto *child:parent->childItems())if(auto *match=visualItem(child,name))return match;
    return nullptr;
}

class BackendTest : public QObject {
    Q_OBJECT
private slots:
    void initTestCase() {qmlRegisterType<EntryModel>("PlexFreq",1,0,"EntryModel");}
    void sharedNavigationOwnsCapabilitiesAndDispatch() {
        NavigationBackend backend;backend.data.insert("serverUrl","http://fixture.invalid");
        QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine);component.setData(R"(import QtQuick 2.6
import "qrc:/tests" as Shared
Item {
    property alias music:session
    property alias catalogue:navigation
    property alias retained:retained
    Shared.Session {id:session}
    Shared.Navigation {id:navigation;music:session}
    Shared.BrowsePageView {id:retained;music:session}
})",QUrl("qrc:/tests/SharedNavigation.qml"));QVERIFY2(component.isReady(),qPrintable(component.errorString()));
        std::unique_ptr<QObject> scene(component.create());QVERIFY(scene.get());
        auto *music=scene->property("music").value<QObject *>();auto *navigation=scene->property("catalogue").value<QObject *>();auto *retained=scene->property("retained").value<QObject *>();QVERIFY(music && navigation && retained);
        auto value=[&](const QString &source){QQmlExpression expression(engine.rootContext(),navigation,source);const auto result=expression.evaluate();if(expression.hasError())qWarning()<<expression.error();return result;};
        auto activate=[&](const QString &id){return QMetaObject::invokeMethod(navigation,"activate",Q_ARG(QVariant,QVariant(id)));};
        QCOMPARE(value("primaryDestinations.join(',')").toString(),QString("discover,library,playlists"));
        QVERIFY(!value("definition('discover').enabled").toBool());QVERIFY(!value("definition('downloaded').enabled").toBool());
        QSignalSpy connection(navigation,SIGNAL(connectionRequested()));QVERIFY(activate("connection"));QCOMPARE(connection.size(),1);
        emit backend.completed("libraries",true,QVariantMap{{"libraries",QVariantList{QVariantMap{{"key","1"},{"title","Music"}}}}});
        QCOMPARE(navigation->property("currentDestination").toString(),QString("discover"));QVERIFY(value("definition('customize').visible").toBool());
        QVERIFY(activate("library"));QCOMPARE(backend.operation,QString("browse"));QVERIFY(!value("definition('back').visible").toBool());QVERIFY(value("definition('filters').visible").toBool());
        QSignalSpy customize(music,SIGNAL(openDiscoverySettings()));QVERIFY(activate("customize"));QCOMPARE(customize.size(),0);
        const auto commands=backend.commandCount;backend.busyValue=true;emit backend.busyChanged();QVERIFY(activate("added"));QCOMPARE(backend.commandCount,commands);
        backend.busyValue=false;emit backend.busyChanged();QVERIFY(activate("added"));QCOMPARE(backend.operation,QString("collection"));QCOMPARE(backend.arguments.value("view").toString(),QString("added"));
        backend.cacheData.insert("tracks",2);emit backend.cacheChanged();QVERIFY(value("definition('downloaded').enabled").toBool());
        backend.data.insert("offlineMode",true);emit backend.stateChanged();QVERIFY(!value("definition('createSmart').enabled").toBool());QVERIFY(value("definition('savedAlbums').visible").toBool());
        const auto offlineCommands=backend.commandCount;QVERIFY(activate("createSmart"));QVERIFY(activate("unknown"));QCOMPARE(backend.commandCount,offlineCommands);
        QVERIFY(QMetaObject::invokeMethod(navigation,"setOffline",Q_ARG(QVariant,QVariant(false))));QCOMPARE(backend.operation,QString("offline_mode"));QCOMPARE(backend.arguments.value("enabled").toBool(),false);
        backend.data.insert("offlineMode",false);emit backend.stateChanged();QVERIFY(activate("queue"));QVERIFY(music->property("showQueue").toBool());QVERIFY(value("definition('back').visible").toBool());
        QSignalSpy back(navigation,SIGNAL(backRequested()));QVERIFY(activate("back"));QVERIFY(!music->property("showQueue").toBool());QCOMPARE(back.size(),0);
        QVERIFY(activate("discover"));QVERIFY(!music->property("canGoBack").toBool());QVERIFY(activate("library"));
        QVERIFY(QMetaObject::invokeMethod(retained,"freeze"));navigation->setProperty("view",QVariant::fromValue(retained));
        QVERIFY(QMetaObject::invokeMethod(music,"home"));QVERIFY(music->property("homeView").toBool());QCOMPARE(navigation->property("currentDestination").toString(),QString("library"));QVERIFY(!value("definition('customize').visible").toBool());
        navigation->setProperty("interactive",false);const auto inactiveCommands=backend.commandCount;QVERIFY(activate("playlists"));QCOMPARE(backend.commandCount,inactiveCommands);
        navigation->setProperty("interactive",true);navigation->setProperty("rootNavigation",false);QVERIFY(!value("definition('played').visible").toBool());QVERIFY(activate("played"));QCOMPARE(backend.commandCount,inactiveCommands);
    }
    void desktopNavigationRendersSharedCatalogue() {
#if QT_VERSION < QT_VERSION_CHECK(6, 0, 0)
        QSKIP("Desktop Controls2 adapter is checked with Qt6");
#else
        NavigationBackend backend;backend.data.insert("serverUrl","http://fixture.invalid");backend.data.insert("libraries",QVariantList{QVariantMap{{"key","1"},{"title","Music"}}});
        QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/qml/desktop/Main.qml"));QVERIFY2(component.isReady(),qPrintable(component.errorString()));
        std::unique_ptr<QObject> window(component.create());QVERIFY(window.get());
        auto *music=window->findChild<QObject *>("musicSession");auto *navigation=window->findChild<QObject *>("navigationDefinitions");QVERIFY(music && navigation);music->setProperty("section","1");
        auto *quickWindow=qobject_cast<QQuickWindow *>(window.get());QVERIFY(quickWindow);QTRY_VERIFY(visualItem(quickWindow->contentItem(),"discoverDestination"));
        auto *discover=visualItem(quickWindow->contentItem(),"discoverDestination");auto *library=visualItem(quickWindow->contentItem(),"libraryDestination");auto *playlists=visualItem(quickWindow->contentItem(),"playlistsDestination");QVERIFY(discover && library && playlists);
        QTRY_COMPARE(discover->width(),library->width());QCOMPARE(library->height(),playlists->height());
        auto *queue=window->findChild<QObject *>("nav_queue");auto *filters=window->findChild<QObject *>("nav_filters");QVERIFY(queue && filters);QVERIFY(!queue->property("checkable").toBool());
        auto *actions=window->findChild<QObject *>("pageActionsMenu");auto *destinations=window->findChild<QObject *>("destinationsMenu");QVERIFY(actions && destinations);QCOMPARE(actions->property("count").toInt(),11);QCOMPARE(destinations->property("count").toInt(),9);
        QVERIFY(QMetaObject::invokeMethod(actions,"open"));QTRY_VERIFY(actions->property("visible").toBool());
        QVERIFY(QMetaObject::invokeMethod(discover,"clicked"));QCOMPARE(backend.operation,QString("discovery_home"));QVERIFY(!filters->property("visible").toBool());
        const QVariant addedA=QVariantMap{{"type","album"},{"ratingKey","40"},{"title","New A"},{"discoveryGroup","Recently added"},{"hubIdentifier","music.recentlyAdded"},{"discoveryGrid",true}};
        const QVariant addedB=QVariantMap{{"type","album"},{"ratingKey","41"},{"title","New B"},{"discoveryGroup","Recently added"},{"hubIdentifier","music.recentlyAdded"},{"discoveryGrid",true}};
        const QVariant rotation=QVariantMap{{"type","track"},{"ratingKey","42"},{"title","Rotation"},{"discoveryGroup","Heavy rotation"},{"hubIdentifier","music.rotation"},{"discoveryGrid",false}};
        const QVariantList discoveryItems{addedA,addedB,rotation};backend.itemModel.replace(discoveryItems);backend.data.insert("items",discoveryItems);emit backend.stateChanged();emit backend.completed("discovery_home",true,QVariantMap{{"start",0}});
        QQuickItem *discoveryGrid=nullptr,*discoveryHost=nullptr;QTRY_VERIFY((discoveryGrid=visualItem(quickWindow->contentItem(),"discoverySectionGrid"))!=nullptr);QTRY_VERIFY((discoveryHost=visualItem(quickWindow->contentItem(),"discoverySectionHost"))!=nullptr);QVERIFY(discoveryGrid->clip());QVERIFY(discoveryHost->clip());QVERIFY(discoveryHost->height()>=discoveryGrid->height()+12);
        QVERIFY(QMetaObject::invokeMethod(library,"clicked"));QCOMPARE(backend.operation,QString("browse"));QVERIFY(filters->property("visible").toBool());QVERIFY(!window->property("showBack").toBool());
        QVariantList artists;for(int i=0;i<60;++i)artists.append(QVariantMap{{"type","artist"},{"ratingKey",QString::number(100+i)},{"title",QString("Artist %1").arg(i)}});
        backend.itemModel.replace(artists);backend.data.insert("items",artists);backend.data.insert("alphabetSection","1");backend.data.insert("alphabet",QVariantList{QVariantMap{{"letter","A"},{"offset",0}},QVariantMap{{"letter","B"},{"offset",30}}});emit backend.stateChanged();emit backend.completed("browse",true,QVariantMap{{"start",0}});
        auto *list=window->findChild<QQuickItem *>("libraryList");auto *rail=window->findChild<QObject *>("libraryAlphabet");QVERIFY(list && rail);QTRY_VERIFY(rail->property("visible").toBool());QTRY_VERIFY(list->property("contentHeight").toReal()>list->height());
        QVERIFY(QMetaObject::invokeMethod(music,"jump",Q_ARG(QVariant,QVariant("B"))));QTRY_VERIFY(list->property("contentY").toReal()>list->property("originY").toReal());QTRY_COMPARE(rail->property("current").toString(),QString("B"));
        list->setProperty("contentY",list->property("originY"));QTRY_COMPARE(rail->property("current").toString(),QString("A"));
        backend.busyValue=true;emit backend.busyChanged();QVERIFY(!library->property("enabled").toBool());QCOMPARE(visualItem(quickWindow->contentItem(),"libraryDestination"),library);
        backend.busyValue=false;emit backend.busyChanged();QVERIFY(QMetaObject::invokeMethod(queue,"triggered"));QVERIFY(music->property("showQueue").toBool());QVERIFY(window->property("showBack").toBool());
        QVERIFY(QMetaObject::invokeMethod(playlists,"clicked"));QCOMPARE(backend.operation,QString("playlists"));QVERIFY(!window->property("showBack").toBool());
#endif
    }
    void stackedBrowsePagesKeepTheirPresentationDuringNavigation() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/tests/PageNavigation.qml"));QVERIFY2(component.isReady(),qPrintable(component.errorString()));
        std::unique_ptr<QObject> scene(component.create());QVERIFY(scene.get());
        auto *music=scene->property("music").value<QObject *>();auto *root=scene->property("rootView").value<QObject *>();auto *artistPage=scene->property("artistView").value<QObject *>();auto *albumPage=scene->property("albumView").value<QObject *>();QVERIFY(music && root && artistPage && albumPage);
        const QVariant artist=QVariantMap{{"type","artist"},{"ratingKey","10"},{"title","Artist"}},album=QVariantMap{{"type","album"},{"ratingKey","20"},{"title","Album"}},track=QVariantMap{{"type","track"},{"ratingKey","30"},{"title","Track"}};
        QVERIFY(QMetaObject::invokeMethod(music,"browse"));backend.data.insert("items",QVariantList{artist});emit backend.stateChanged();
        QVERIFY(QMetaObject::invokeMethod(music,"activate",Q_ARG(QVariant,artist),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(root->property("heading").toString(),QString("Artists"));QVERIFY(root->property("detail").toMap().isEmpty());QCOMPARE(root->property("items").toList(),QVariantList{artist});
        backend.data.insert("items",QVariantList{album});emit backend.stateChanged();emit backend.completed("detail",true,QVariantMap{{"detail",artist},{"start",0}});
        QCOMPARE(artistPage->property("detail").toMap().value("ratingKey").toString(),QString("10"));
        QVERIFY(QMetaObject::invokeMethod(music,"activate",Q_ARG(QVariant,album),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(artistPage->property("heading").toString(),QString("Artist"));QCOMPARE(artistPage->property("detail").toMap().value("ratingKey").toString(),QString("10"));QCOMPARE(artistPage->property("items").toList(),QVariantList{album});
        backend.data.insert("items",QVariantList{track});emit backend.stateChanged();emit backend.completed("detail",true,QVariantMap{{"detail",album},{"start",0}});
        QCOMPARE(albumPage->property("detail").toMap().value("ratingKey").toString(),QString("20"));QCOMPARE(albumPage->property("items").toList(),QVariantList{track});
        // These are the retained pages visible behind a pop/peek animation.
        QCOMPARE(root->property("items").toList(),QVariantList{artist});QCOMPARE(artistPage->property("items").toList(),QVariantList{album});
        auto *rows=qobject_cast<QAbstractItemModel *>(root->property("rows").value<QObject *>());QVERIFY(rows);QCOMPARE(rows->rowCount(),1);QCOMPARE(rows->data(rows->index(0,0),Qt::UserRole+1),artist);
        QVERIFY(QMetaObject::invokeMethod(albumPage,"freeze"));QVERIFY(QMetaObject::invokeMethod(music,"back"));
        backend.data.insert("items",QVariantList());emit backend.stateChanged();
        QCOMPARE(artistPage->property("items").toList(),QVariantList{album}); // keep the back-animation target while reloading
        backend.data.insert("items",QVariantList{album});emit backend.stateChanged();emit backend.completed("detail",true,QVariantMap{{"detail",artist},{"start",0}});
        QVERIFY(QMetaObject::invokeMethod(artistPage,"thaw"));QCOMPARE(artistPage->property("detail").toMap().value("ratingKey").toString(),QString("10"));
        QCOMPARE(albumPage->property("items").toList(),QVariantList{track});
        QCOMPARE(qobject_cast<QAbstractItemModel *>(root->property("rows").value<QObject *>()),rows);
    }
    void sailfishStackAnimationKeepsOutgoingPageContent() {
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
        QSKIP("Actual Silica stack is checked by the Qt5 phone fixture");
#else
        NavigationBackend backend;backend.data.insert("libraries",QVariantList());QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent controller(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(controller.isReady());std::unique_ptr<QObject> music(controller.create());QVERIFY(music.get());music->setProperty("section","1");QVERIFY(QMetaObject::invokeMethod(music.get(),"browse"));
        QQmlComponent component(&engine);component.setData(R"(import QtQuick 2.6
import Sailfish.Silica 1.0
import "qrc:/qml/pages" as Pages
ApplicationWindow {
    id:app
    property var music
    property var current:pageStack.currentPage
    property bool transitioning:pageStack.busy
    function pop() {pageStack.pop()}
    initialPage:Component {Pages.LibraryPage {music:app.music}}
    Connections {target:app.music;onNavigate:app.pageStack.push(Qt.resolvedUrl(kind==="artist" ? "qrc:/qml/pages/ArtistPage.qml" : "qrc:/qml/pages/AlbumPage.qml"),{music:app.music,pageKey:key})}
})",QUrl("qrc:/tests/StackNavigation.qml"));QVERIFY2(component.isReady(),qPrintable(component.errorString()));
        QQuickView window(&engine,nullptr);window.setResizeMode(QQuickView::SizeRootObjectToView);std::unique_ptr<QObject> appOwner(component.beginCreate(engine.rootContext()));QVERIFY(appOwner.get());auto *app=appOwner.get();app->setProperty("music",QVariant::fromValue(music.get()));component.completeCreate();QVERIFY(!component.isError());
        auto *item=qobject_cast<QQuickItem *>(app);QVERIFY(item);window.setContent(QUrl("qrc:/tests/StackNavigation.qml"),&component,item);appOwner.release();item->setVisible(true);window.resize(app->property("_screenWidth").toInt(),app->property("_screenHeight").toInt());window.show();
        QTRY_VERIFY(app->property("current").value<QObject *>());QTRY_VERIFY(!app->property("transitioning").toBool());
        QObject *root=app->property("current").value<QObject *>();auto *rootView=root->findChild<QObject *>("browsePageView");QVERIFY(rootView);
        const QVariant artist=QVariantMap{{"type","artist"},{"ratingKey","10"},{"title","Artist"}},album=QVariantMap{{"type","album"},{"ratingKey","20"},{"title","Album"}};
        backend.data.insert("items",QVariantList{artist});emit backend.stateChanged();
        QVERIFY(QMetaObject::invokeMethod(music.get(),"activate",Q_ARG(QVariant,artist),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(rootView->property("heading").toString(),QString("Artists"));QVERIFY(rootView->property("detail").toMap().isEmpty());QCOMPARE(rootView->property("items").toList(),QVariantList{artist});
        QTRY_VERIFY(app->property("current").value<QObject *>()!=root);QTRY_VERIFY(!app->property("transitioning").toBool());
        auto *artistView=app->property("current").value<QObject *>()->findChild<QObject *>("browsePageView");QVERIFY(artistView);
        backend.data.insert("items",QVariantList{album});emit backend.stateChanged();emit backend.completed("detail",true,QVariantMap{{"detail",artist},{"start",0}});
        QVERIFY(QMetaObject::invokeMethod(music.get(),"activate",Q_ARG(QVariant,album),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(artistView->property("detail").toMap().value("ratingKey").toString(),QString("10"));QCOMPARE(artistView->property("items").toList(),QVariantList{album});
        QCOMPARE(rootView->property("items").toList(),QVariantList{artist});QTRY_VERIFY(!app->property("transitioning").toBool());
        QVERIFY(QMetaObject::invokeMethod(app,"pop"));
        backend.data.insert("items",QVariantList());emit backend.stateChanged();
        QCOMPARE(artistView->property("items").toList(),QVariantList{album});
        QTRY_VERIFY(app->property("current").value<QObject *>() && app->property("current").value<QObject *>()->findChild<QObject *>("browsePageView")==artistView);QTRY_VERIFY(!app->property("transitioning").toBool());
        QCOMPARE(backend.arguments.value("key").toString(),QString("10"));QVERIFY(artistView->property("frozen").toBool());
        backend.data.insert("items",QVariantList{album});emit backend.stateChanged();emit backend.completed("detail",true,QVariantMap{{"detail",artist},{"start",0}});
        QVERIFY(!artistView->property("frozen").toBool());QCOMPARE(artistView->property("detail").toMap().value("ratingKey").toString(),QString("10"));
        QVERIFY(QMetaObject::invokeMethod(app,"pop"));QTRY_COMPARE(app->property("current").value<QObject *>(),root);QTRY_VERIFY(!app->property("transitioning").toBool());
        backend.data.insert("items",QVariantList{artist});emit backend.stateChanged();emit backend.completed("browse",true,QVariantMap{{"start",0}});
        QVERIFY(!rootView->property("frozen").toBool());
        // Use the shipped destination control, not just the Session method.
        auto *discover=visualItem(qobject_cast<QQuickItem *>(root),"discoverDestination");QVERIFY(discover);
        auto *discoverLabel=visualItem(discover,"destinationLabel");QVERIFY(discoverLabel);QVERIFY(!discoverLabel->property("text").toString().isEmpty());
        void *mouseEvent=nullptr;
        QVERIFY(QMetaObject::invokeMethod(discover,"clicked",QGenericArgument("QQuickMouseEvent*",&mouseEvent)));QCOMPARE(backend.operation,QString("discovery_home"));
        QVERIFY(QMetaObject::invokeMethod(rootView,"freeze"));backend.busyValue=true;emit backend.busyChanged();
        const QVariant discoveryAlbum=QVariantMap{{"type","album"},{"ratingKey","40"},{"title","Discovery album"},{"discoveryGroup","Recently added"},{"hubIdentifier","music.recentlyAdded"},{"discoveryGrid",true}};
        backend.data.insert("items",QVariantList{discoveryAlbum});emit backend.stateChanged();emit backend.completed("discovery_home",true,QVariantMap{{"start",0}});
        QTRY_VERIFY(rootView->property("homeView").toBool());QCOMPARE(rootView->property("heading").toString(),QString("Discover"));QCOMPARE(rootView->property("items").toList(),QVariantList{discoveryAlbum});
        QTRY_VERIFY(root->findChild<QObject *>("discoverySectionGrid"));
        QVERIFY(!rootView->property("frozen").toBool());backend.busyValue=false;emit backend.busyChanged();
        QVERIFY(!root->property("libraryView").toBool());
        QTest::qWait(500);QCOMPARE(backend.operation,QString("discovery_home"));
        auto *library=visualItem(qobject_cast<QQuickItem *>(root),"libraryDestination");QVERIFY(library);QVERIFY(QMetaObject::invokeMethod(library,"clicked",QGenericArgument("QQuickMouseEvent*",&mouseEvent)));QCOMPARE(backend.operation,QString("browse"));
        QVariantList artists;for(int i=0;i<60;++i)artists.append(QVariantMap{{"type","artist"},{"ratingKey",QString::number(100+i)},{"title",QString("Artist %1").arg(i)}});
        backend.data.insert("serverUrl","http://fixture.invalid");backend.data.insert("items",artists);backend.data.insert("alphabetSection","1");backend.data.insert("alphabet",QVariantList{QVariantMap{{"letter","A"},{"offset",0}},QVariantMap{{"letter","B"},{"offset",30}}});emit backend.stateChanged();emit backend.completed("browse",true,QVariantMap{{"start",0}});
        // Use the production QQuickView/Silica hierarchy for geometry too.
        // The isolated-bus fixture has no native application-visibility owner.
        for(auto *ancestor=qobject_cast<QQuickItem *>(root);ancestor;ancestor=ancestor->parentItem()) {
            QQmlExpression visible(engine.rootContext(),ancestor,"visible = true");visible.evaluate();QVERIFY(!visible.hasError());
        }
        auto *list=root->findChild<QQuickItem *>("libraryList");auto *header=root->findChild<QQuickItem *>("libraryHeader");auto *rail=root->findChild<QQuickItem *>("libraryAlphabet");auto *toggle=root->findChild<QObject *>("libraryControlsToggle");QVERIFY(list && header && rail && toggle);
        QTRY_VERIFY(rail->isVisible());QCOMPARE(list->width(),root->property("width").toReal());QCOMPARE(header->width(),list->width());QCOMPARE(rail->parentItem(),list);
        QCOMPARE(list->property("headerPositioning").toInt(),0); // InlineHeader
        list->setProperty("contentY",header->y());QQuickWindowPrivate::get(&window)->polishItems();const auto collapsedHeight=header->height();QVERIFY(!root->property("controlsExpanded").toBool());
        QVERIFY(QMetaObject::invokeMethod(toggle,"clicked",QGenericArgument("QQuickMouseEvent*",&mouseEvent)));QVERIFY(root->property("controlsExpanded").toBool());
        auto *controlsLoader=root->findChild<QObject *>("libraryControlsLoader");QVERIFY(controlsLoader);QTRY_VERIFY(controlsLoader->property("item").value<QObject *>());QQuickWindowPrivate::get(&window)->polishItems();QVERIFY(header->height()>collapsedHeight);
        QVERIFY(QMetaObject::invokeMethod(toggle,"clicked",QGenericArgument("QQuickMouseEvent*",&mouseEvent)));QQuickWindowPrivate::get(&window)->polishItems();QCOMPARE(header->height(),collapsedHeight);
        QVERIFY(QMetaObject::invokeMethod(music.get(),"jump",Q_ARG(QVariant,QVariant("B"))));QTRY_VERIFY(list->property("contentY").toReal()>list->property("originY").toReal());QTRY_COMPARE(rail->property("current").toString(),QString("B"));
        list->setProperty("contentY",list->property("originY"));QTRY_COMPARE(rail->property("current").toString(),QString("A"));
        const auto top=list->property("contentY").toReal();list->setProperty("contentY",top+1200);QTest::qWait(100);list->setProperty("contentY",top+600);QTest::qWait(100);list->setProperty("contentY",top);QTest::qWait(100);
        QVERIFY(rail->y()>=header->y()+header->height()-list->property("contentY").toReal());QVERIFY(rail->y()+rail->height()<=list->height());QCOMPARE(header->width(),list->width());
#endif
    }
    void translationCataloguesLoadAndPreservePlaceholders() {
        const auto languages=QString("bg bn cs da de el es et fi fr gu hi hu it kn lt lv ml mr nb nl pa pl pt pt_BR ro ru sk sl sv ta te tr tt uk vi zh_CN zh_HK zh_TW").split(' ');
        for(const auto &language:languages){QTranslator translator;QVERIFY(translator.load(":/translations/harbour-plexfreq_"+language+".qm"));QCoreApplication::installTranslator(&translator);
            const auto text=QCoreApplication::translate("Session","%1 tracks");QVERIFY(text.contains("%1"));QVERIFY(!text.isEmpty());
            const auto error=translatedMessage("Plex returned HTTP 404");QVERIFY(error.contains("404"));QVERIFY(!error.contains("%1"));
            if(language=="it")QCOMPARE(QCoreApplication::translate("LibraryPage","Search artists, albums and tracks"),QString::fromUtf8("Cerca artisti, album e brani"));
            QCoreApplication::removeTranslator(&translator);
        }
    }
    void translationSelectionUsesConfiguredLocaleWithoutLosingOverrides() {
        QTemporaryDir directory;QVERIFY(directory.isValid());const auto path=directory.path()+"/locale.conf";
        QFile config(path);QVERIFY(config.open(QIODevice::WriteOnly));config.write("# Synthetic locale only\nLANG=\"it_IT.utf8\"\n");config.close();
        QCOMPARE(translationLocale("","C",path),QString("it_IT"));
        QCOMPARE(translationLocale("de-DE.UTF-8","C",path),QString("de_DE"));
        QCOMPARE(translationLocale("C","it_IT",path),QString("C"));
        QCOMPARE(translationLocale("","en_US"),QString("en_US"));
        QVERIFY(config.open(QIODevice::WriteOnly|QIODevice::Truncate));config.write("LANG=it_IT.UTF-8\nLC_MESSAGES=en_GB.UTF-8\n");config.close();
        QCOMPARE(translationLocale("","C",path),QString("en_GB"));
        auto *app=QCoreApplication::instance();const auto previous=app->findChildren<QTranslator *>();const auto language=qgetenv("PLEXFREQ_LANGUAGE");
        qputenv("PLEXFREQ_LANGUAGE","it_IT.utf8");installTranslations(app);
        const auto discover=QCoreApplication::translate("Navigation","Discover");const auto showAll=QCoreApplication::translate("Navigation","Show all");
        for(auto *translator:app->findChildren<QTranslator *>())if(!previous.contains(translator)){app->removeTranslator(translator);delete translator;}
        if(language.isNull())qunsetenv("PLEXFREQ_LANGUAGE");else qputenv("PLEXFREQ_LANGUAGE",language);
        QCOMPARE(discover,QString::fromUtf8("Scopri"));QCOMPARE(showAll,QString::fromUtf8("Mostra tutto"));
    }
    void configuredPhoneLanguageLoadsWithoutShellLocale() {
#ifndef SAILFISH
        QSKIP("Configured Sailfish locale check runs on the phone");
#else
        if(qgetenv("PLEXFREQ_SYSTEM_LOCALE_CHECK").isEmpty())QSKIP("Opt-in read-only phone locale check");
        const auto configured=translationLocale("","C","/etc/locale.conf");QVERIFY(configured.startsWith("it"));
        const QList<QByteArray> names{"PLEXFREQ_LANGUAGE","LC_ALL","LC_MESSAGES","LANG","LANGUAGE"};QList<QByteArray> values;
        for(const auto &name:names){values.append(qgetenv(name.constData()));qunsetenv(name.constData());}
        auto *app=QCoreApplication::instance();const auto previous=app->findChildren<QTranslator *>();installTranslations(app);
        const auto discover=QCoreApplication::translate("Navigation","Discover");
        for(auto *translator:app->findChildren<QTranslator *>())if(!previous.contains(translator)){app->removeTranslator(translator);delete translator;}
        for(int i=0;i<names.size();++i){if(values[i].isNull())qunsetenv(names[i].constData());else qputenv(names[i].constData(),values[i]);}
        QCOMPARE(discover,QString::fromUtf8("Scopri"));
#endif
    }
    void networkHintsBeforeWorkerStartupAreRetained() {
        QTemporaryDir directory;Backend backend(directory.path());QSignalSpy replies(&backend,&Backend::completed);
        QTRY_VERIFY(!backend.busy());backend.command("download_policy",{{"wifi_only",true},{"paused",true}});QTRY_VERIFY(!backend.busy());QTRY_VERIFY(backend.cache().value("paused").toBool());QVERIFY(backend.cache().value("wifiOnly").toBool());
    }
    void managementPagesLoad() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent controller(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(controller.isReady());std::unique_ptr<QObject> session(controller.create());QVERIFY(session.get());
        for(const auto &name:QStringList{"PlaylistDialog","DownloadsPage","AudioSettingsPage","FiltersDialog","DiscoverySettingsPage","InsightsPage"}) {
            QQuickWindow window;window.resize(600,1000);
            QQmlComponent component(&engine);
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
            component.loadUrl(QUrl(name=="PlaylistDialog"?"qrc:/qml/desktop/PlaylistEditor.qml":name=="AudioSettingsPage"?"qrc:/qml/desktop/AudioSettings.qml":name=="FiltersDialog"?"qrc:/qml/desktop/Filters.qml":name=="DiscoverySettingsPage"?"qrc:/qml/desktop/DiscoverySettings.qml":name=="InsightsPage"?"qrc:/qml/desktop/Insights.qml":"qrc:/qml/desktop/Downloads.qml"));
#else
            const auto qml=QString("import QtQuick 2.6\nimport Sailfish.Silica 1.0\nApplicationWindow {id:app;property var music;initialPage:Component {Page {}}\nTimer {interval:1;running:true;onTriggered:app.pageStack.push(Qt.resolvedUrl(\"qrc:/qml/pages/%1.qml\"),{music:app.music})}}").arg(name);
            component.setData(qml.toUtf8(),QUrl("qrc:/tests/Management.qml"));
#endif
            if(!component.isReady())qWarning()<<component.errors();
            QVERIFY(component.isReady());std::unique_ptr<QObject> page(component.beginCreate(engine.rootContext()));QVERIFY(page.get());
            page->setProperty("music",QVariant::fromValue(session.get()));page->setProperty("width",600);component.completeCreate();QVERIFY(!component.isError());
#if QT_VERSION < QT_VERSION_CHECK(6, 0, 0)
            auto *item=qobject_cast<QQuickItem *>(page.get());QVERIFY(item);item->setHeight(1000);item->setParentItem(window.contentItem());window.show();
#endif
            QTest::qWait(100);
        }
    }
    void readOnlyNetworkFacts() {
        QTemporaryDir directory;Backend backend(directory.path());QSignalSpy completed(&backend,&Backend::completed);
        auto seen=[&]{for(const auto &row:completed)if(row[0].toString()=="network_state")return true;return false;};QTRY_VERIFY_WITH_TIMEOUT(seen(),7000);
        qDebug("Network facts: wifi %d, connected %d",backend.state().value("networkWifi").toBool(),backend.state().value("networkOnline").toBool());
    }
    void discoveryQualityAndOfflineActionsUseSharedNavigation() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(component.isReady());std::unique_ptr<QObject> session(component.create());QVERIFY(session.get());
        emit backend.completed("libraries",true,QVariantMap{{"libraries",QVariantList{QVariantMap{{"key","1"},{"title","Music"}}}}});QCOMPARE(backend.operation,QString("discovery_home"));QCOMPARE(backend.arguments.value("section").toString(),QString("1"));QVERIFY(session->property("homeView").toBool());
        QVERIFY(QMetaObject::invokeMethod(session.get(),"browseLibrary",Q_ARG(QVariant,QVariant(true))));QCOMPARE(backend.operation,QString("browse"));QVERIFY(session->property("canGoBack").toBool());
        QVERIFY(QMetaObject::invokeMethod(session.get(),"browseLibrary",Q_ARG(QVariant,QVariant())));
        QVERIFY(QMetaObject::invokeMethod(session.get(),"back"));QCOMPARE(backend.operation,QString("discovery_home"));QVERIFY(!session->property("canGoBack").toBool());
        QVERIFY(QMetaObject::invokeMethod(session.get(),"browseLibrary",Q_ARG(QVariant,QVariant(true))));QVERIFY(QMetaObject::invokeMethod(session.get(),"goHome",Q_ARG(QVariant,QVariant(true))));QVERIFY(!session->property("canGoBack").toBool());QVERIFY(session->property("homeView").toBool());
        backend.data.insert("qualityConfig",QVariantMap{{"wifiKbps",0},{"mobileKbps",160},{"downloadKbps",128},{"codecFallback",true}});emit backend.stateChanged();
        QVERIFY(QMetaObject::invokeMethod(session.get(),"qualitySetting",Q_ARG(QVariant,QVariant("wifiKbps")),Q_ARG(QVariant,QVariant(320))));
        QCOMPARE(backend.operation,QString("quality_config"));const auto config=backend.arguments.value("config").toMap();QCOMPARE(config.value("wifiKbps").toInt(),320);QCOMPARE(config.value("mobileKbps").toInt(),160);QCOMPARE(config.value("downloadKbps").toInt(),128);
        const QVariant start=QVariantMap{{"type","track"},{"ratingKey","1"}},end=QVariantMap{{"type","track"},{"ratingKey","3"}};
        session->setProperty("adventureStart",start);QVERIFY(QMetaObject::invokeMethod(session.get(),"sonicAdventure",Q_ARG(QVariant,end)));QCOMPARE(backend.operation,QString("sonic_adventure"));QCOMPARE(backend.arguments.value("start_key").toString(),QString("1"));QCOMPARE(backend.arguments.value("end_key").toString(),QString("3"));
        session->setProperty("downloadMinutes",120);QVERIFY(QMetaObject::invokeMethod(session.get(),"downloadRadio",Q_ARG(QVariant,start)));QCOMPARE(backend.operation,QString("download_plan"));QCOMPARE(backend.arguments.value("minutes").toInt(),120);QCOMPARE(backend.arguments.value("kind").toString(),QString("track_radio"));
        const QVariant station=QVariantMap{{"type","playlist"},{"station",true},{"key","/library/sections/1/station/1"},{"title","Station"}};
        QVERIFY(QMetaObject::invokeMethod(session.get(),"activate",Q_ARG(QVariant,station),Q_ARG(QVariant,QVariant(0))));QCOMPARE(backend.operation,QString("station"));
        backend.data.insert("offlineMode",true);emit backend.stateChanged();QVERIFY(QMetaObject::invokeMethod(session.get(),"search",Q_ARG(QVariant,QVariant("Offline album"))));QTRY_COMPARE(backend.operation,QString("offline_search"));QCOMPARE(backend.arguments.value("query").toString(),QString("Offline album"));
        EntryModel model;model.replace({QVariantMap{{"ratingKey","1"},{"type","track"},{"discoveryGroup","Heavy rotation"}}});QCOMPARE(model.data(model.index(0),Qt::UserRole+3).toString(),QString("Heavy rotation"));
        const QVariantList homeItems{QVariantMap{{"hubIdentifier","music.recentlyAdded"},{"discoveryGrid",true}},QVariantMap{{"hubIdentifier","music.recentlyAdded"},{"discoveryGrid",true}},QVariantMap{{"hubIdentifier","music.rotation"},{"discoveryGrid",false}}};QVariant cards;
        QVERIFY(QMetaObject::invokeMethod(session.get(),"discoveryGridItems",Q_RETURN_ARG(QVariant,cards),Q_ARG(QVariant,homeItems),Q_ARG(QVariant,QVariant(0))));QCOMPARE(cards.toList().size(),2);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"discoveryGridItems",Q_RETURN_ARG(QVariant,cards),Q_ARG(QVariant,homeItems),Q_ARG(QVariant,QVariant(1))));QCOMPARE(cards.toList().size(),0);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"discovery",Q_ARG(QVariant,QVariant("added"))));QVERIFY(session->property("gridBrowse").toBool());
        QVERIFY(QMetaObject::invokeMethod(session.get(),"discovery",Q_ARG(QVariant,QVariant("played"))));QVERIFY(session->property("gridBrowse").toBool());
        QVERIFY(QMetaObject::invokeMethod(session.get(),"discovery",Q_ARG(QVariant,QVariant("favorites"))));QVERIFY(!session->property("gridBrowse").toBool());
        backend.data.insert("offlineMode",false);emit backend.stateChanged();QVERIFY(QMetaObject::invokeMethod(session.get(),"browse"));
        backend.data.insert("alphabetSection","1");backend.data.insert("alphabet",QVariantList{QVariantMap{{"letter","A"},{"offset",0}},QVariantMap{{"letter","B"},{"offset",30}}});backend.data.insert("hasMore",true);backend.data.insert("next",100);emit backend.stateChanged();emit backend.completed("browse",true,QVariantMap{{"start",0}});QTRY_COMPARE(backend.operation,QString("browse"));QCOMPARE(backend.arguments.value("start").toInt(),100);
        const int beforeJump=backend.commandCount;QSignalSpy jumped(session.get(),SIGNAL(artistJumped(int)));QVERIFY(QMetaObject::invokeMethod(session.get(),"jump",Q_ARG(QVariant,QVariant("B"))));QCOMPARE(backend.commandCount,beforeJump);QCOMPARE(jumped.size(),1);QCOMPARE(jumped.first().first().toInt(),30);
    }
    void filterEditorAndSonicWaypointsKeepStructuredArguments() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);QQmlComponent component(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(component.isReady());std::unique_ptr<QObject> music(component.create());QVERIFY(music.get());music->setProperty("section","1");
        QVERIFY(QMetaObject::invokeMethod(music.get(),"editFilters",Q_ARG(QVariant,QVariant("browse")),Q_ARG(QVariant,QVariant())));
        QVERIFY(QMetaObject::invokeMethod(music.get(),"filterValue",Q_ARG(QVariant,QVariant("genre")),Q_ARG(QVariant,QVariant("7"))));
        QVERIFY(QMetaObject::invokeMethod(music.get(),"submitFilters"));QCOMPARE(backend.operation,QString("filtered_browse"));QCOMPARE(backend.arguments.value("filters").toMap().value("genre").toString(),QString("7"));
        for(const auto &key:QStringList{"1","3","5"})QVERIFY(QMetaObject::invokeMethod(music.get(),"addJourneyTrack",Q_ARG(QVariant,QVariant(QVariantMap{{"type","track"},{"ratingKey",key}}))));
        QVERIFY(QMetaObject::invokeMethod(music.get(),"journey"));QCOMPARE(backend.operation,QString("sonic_journey"));QCOMPARE(backend.arguments.value("keys").toList(),QVariantList({"1","3","5"}));
    }
    void savedExpandedLibraryAndPlaylists() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Read-only browsing/playlist validation is opt-in");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &input){const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),bytes.constData());const auto result=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return result;};
        const auto libraries=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray();QVERIFY(!libraries.isEmpty());const auto section=libraries.first().toObject().value("key").toString();
        for(const auto &kind:QStringList{"album","track"}) {
            const auto result=call({{"op","library_browse"},{"section",section},{"kind",kind},{"sort","year"},{"start",0}});QVERIFY2(result.value("ok").toBool(),qPrintable(result.value("error").toString()));
            qDebug("Expanded %s browse rows: %d",qPrintable(kind),int(result.value("data").toObject().value("items").toArray().size()));
        }
        const auto playlists=call({{"op","playlist_choices"}});QVERIFY2(playlists.value("ok").toBool(),qPrintable(playlists.value("error").toString()));
        const auto choices=playlists.value("data").toObject().value("playlistChoices").toArray();int occurrences=0,smart=0;
        for(const auto &item:choices){const auto page=call({{"op","playlist_items"},{"key",item.toObject().value("ratingKey").toString()},{"start",0}});QVERIFY2(page.value("ok").toBool(),qPrintable(page.value("error").toString()));const auto metadata=page.value("data").toObject().value("playlist").toObject();if(metadata.value("smart").toBool())++smart;for(const auto &track:page.value("data").toObject().value("items").toArray()){if(!track.toObject().value("playlistItemId").isNull())++occurrences;else QVERIFY(metadata.value("smart").toBool());}}
        qDebug("Playlist chooser count %d; smart playlists %d; regular first-page occurrence IDs %d",int(choices.size()),smart,occurrences);
    }
    void savedDailyDiscoveryAndBoundedTranscode() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Opt-in read-only discovery and at most 256 KiB of transcoded audio");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &input){const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),bytes.constData());const auto envelope=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return envelope;};
        const auto libraries=call({{"op","libraries"}});QVERIFY2(libraries.value("ok").toBool(),qPrintable(libraries.value("error").toString()));const auto sections=libraries.value("data").toObject().value("libraries").toArray();QVERIFY(!sections.isEmpty());const auto section=sections.first().toObject().value("key").toString();
        const auto home=call({{"op","discovery_home"},{"section",section}});QVERIFY2(home.value("ok").toBool(),qPrintable(home.value("error").toString()));const auto items=home.value("data").toObject().value("items").toArray();int stations=0;for(const auto &item:items)if(item.toObject().value("station").toBool())++stations;qDebug("Daily discovery: %d rows; %d stations",int(items.size()),stations);
        const auto tracks=call({{"op","browse"},{"section",section},{"kind","track"},{"start",0}});QVERIFY2(tracks.value("ok").toBool(),qPrintable(tracks.value("error").toString()));QString key;
        for(const auto &item:tracks.value("data").toObject().value("items").toArray()){const auto track=item.toObject();const auto duration=track.value("duration").toDouble();if(duration>0 && duration<=600000){key=track.value("ratingKey").toString();break;}}
        QVERIFY(!key.isEmpty());const auto probe=call({{"op","probe_quality"},{"key",key},{"kbps",160}});QVERIFY2(probe.value("ok").toBool(),qPrintable(probe.value("error").toString()));const auto data=probe.value("data").toObject().value("transcodeProbe").toObject();QVERIFY(data.value("bytes").toInt()>0);QVERIFY(data.value("bytes").toInt()<=256*1024);QVERIFY(data.value("mime").toString().startsWith("audio/"));QVERIFY(data.value("stopped").toBool());qDebug("Transcode probe: %d bytes; cleanup acknowledged",data.value("bytes").toInt());
    }
    void savedDiscoveryGridIdentifiers() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Opt-in read-only discovery hub identifier check");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &input){const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),bytes.constData());const auto envelope=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return envelope;};
        const auto libraries=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray();QVERIFY(!libraries.isEmpty());const auto section=libraries.first().toObject().value("key").toString();
        const auto reply=call({{"op","discovery_home"},{"section",section}});QVERIFY2(reply.value("ok").toBool(),qPrintable(reply.value("error").toString()));const auto data=reply.value("data").toObject();QStringList identifiers;for(const auto &hub:data.value("discoveryHubs").toArray())identifiers.append(hub.toObject().value("identifier").toString());int cards=0;for(const auto &item:data.value("items").toArray())if(item.toObject().value("discoveryGrid").toBool())++cards;
        QVERIFY2(cards>0,qPrintable(QString("No recent grid hub matched identifiers: %1").arg(identifiers.join(','))));qDebug("Discovery grid cards: %d; hub identifiers: %s",cards,qPrintable(identifiers.join(',')));
    }
    void savedTranscodedPlaybackAndSeek() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Opt-in brief fake-sink transcode playback/seek; no listening events");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &input){const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),bytes.constData());const auto envelope=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return envelope;};
        const auto libs=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray();QVERIFY(!libs.isEmpty());const auto section=libs.first().toObject().value("key").toString();const auto page=call({{"op","browse"},{"section",section},{"kind","track"},{"start",0}});QVERIFY(page.value("ok").toBool());QString key;
        for(const auto &item:page.value("data").toObject().value("items").toArray()){const auto track=item.toObject();if(track.value("duration").toDouble()>=10000 && track.value("duration").toDouble()<=600000){key=track.value("ratingKey").toString();break;}}
        QVERIFY(!key.isEmpty());const auto result=call({{"op","probe_playback"},{"key",key},{"kbps",160}});QVERIFY2(result.value("ok").toBool(),qPrintable(result.value("error").toString()));const auto data=result.value("data").toObject().value("playbackProbe").toObject();QVERIFY(data.value("played").toBool());QVERIFY(data.value("paused").toBool());QVERIFY(data.value("seeked").toBool());QVERIFY(data.value("listened").toInt()<2000);qDebug("Transcoded playback/seek passed; heard milliseconds %d",data.value("listened").toInt());
    }
    void savedFilterChoicesAndFilteredBrowse() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Opt-in read-only filter metadata and browsing");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());auto call=[&](const QVariantMap &input){const auto bytes=QJsonDocument::fromVariant(input).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),bytes.constData());const auto envelope=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return envelope;};
        const auto libraries=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray();QVERIFY(!libraries.isEmpty());const auto section=libraries.first().toObject().value("key").toString();QString genre;
        for(const auto &field:QStringList{"genre","mood","style"}){const auto reply=call({{"op","filter_options"},{"section",section},{"kind","track"},{"field",field}});QVERIFY2(reply.value("ok").toBool(),qPrintable(reply.value("error").toString()));const auto choices=reply.value("data").toObject().value("filterChoices").toArray();qDebug("Filter %s: %d choices",qPrintable(field),int(choices.size()));if(field=="genre" && !choices.isEmpty())genre=choices.first().toObject().value("key").toString();}
        const auto page=call({{"op","filtered_browse"},{"section",section},{"kind","track"},{"sort","title"},{"filters",QVariantMap{{"genre",genre},{"yearFrom",2000}}},{"start",0}});QVERIFY2(page.value("ok").toBool(),qPrintable(page.value("error").toString()));qDebug("Filtered browse: %d rows",int(page.value("data").toObject().value("items").toArray().size()));
    }
    void playlistSummarySurvivesTrackNavigation() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(component.isReady());
        std::unique_ptr<QObject> session(component.create());QVERIFY(session.get());
        const QVariant playlist=QVariantMap{{"ratingKey","7"},{"key","/playlists/7/items"},{"type","playlist"},{"title","Fixture playlist"},{"leafCount",150},{"totalDuration",7265000}};
        QVariant summary;QVERIFY(QMetaObject::invokeMethod(session.get(),"playlistSummary",Q_RETURN_ARG(QVariant,summary),Q_ARG(QVariant,playlist)));
        QCOMPARE(summary.toString(),QString("150 tracks · 2:01:05"));
        QVERIFY(QMetaObject::invokeMethod(session.get(),"activate",Q_ARG(QVariant,playlist),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(backend.operation,QString("playlist_items"));QCOMPARE(session->property("playlist").toMap().value("leafCount").toInt(),150);
        QVariant emptySummary;QVERIFY(QMetaObject::invokeMethod(session.get(),"playlistSummary",Q_RETURN_ARG(QVariant,emptySummary),Q_ARG(QVariant,QVariant(QVariantMap{{"leafCount",0},{"totalDuration",0}}))));
        QCOMPARE(emptySummary.toString(),QString("0 tracks · 0:00"));
    }
    void nowPlayingPageLoadsWithLyrics() {
        NavigationBackend backend;backend.data.insert("track",QVariantMap{{"ratingKey","3"},{"title","Fixture song"},{"parentTitle","Fixture album"},{"grandparentTitle","Fixture artist"}});
        QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent sessionComponent(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(sessionComponent.isReady());
        std::unique_ptr<QObject> session(sessionComponent.create());QVERIFY(session.get());
        session->setProperty("lyrics",QVariantList{QVariantMap{{"time",1000},{"text","Fixture lyric"}}});
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
        QQmlComponent component(&engine,QUrl("qrc:/qml/desktop/NowPlaying.qml"));
#else
        QQmlComponent component(&engine);
        component.setData(R"(import QtQuick 2.6
import Sailfish.Silica 1.0
import "qrc:/qml/pages" as Pages
ApplicationWindow {
    id:app
    property var music
    initialPage:Component {Pages.NowPlayingPage {music:app.music}}
})",QUrl("qrc:/tests/NowPlayingWindow.qml"));
#endif
        if(!component.isReady())qWarning()<<component.errors();
        QVERIFY(component.isReady());
        std::unique_ptr<QObject> page(component.beginCreate(engine.rootContext()));QVERIFY(page.get());
        page->setProperty("music",QVariant::fromValue(session.get()));page->setProperty("width",600);
        component.completeCreate();QVERIFY(!component.isError());
        QCOMPARE(page->property("music").value<QObject *>(),session.get());
        QObject *slider=nullptr;QTRY_VERIFY((slider=page->findChild<QObject *>("playbackSlider"))!=nullptr);
        QQmlExpression drag(engine.rootContext(),slider,"value=8000");drag.evaluate();QVERIFY(!drag.hasError());
        backend.playbackPosition=50;backend.data.insert("track",QVariantMap{{"ratingKey","4"}});emit backend.stateChanged();emit backend.playbackChanged();
        QTRY_COMPARE(slider->property("value").toDouble(),50.);
        backend.playbackPosition=400;emit backend.playbackChanged();QTRY_COMPARE(slider->property("value").toDouble(),400.);
    }
    void mprisSessionBusContract() {
        QVERIFY(QDBusConnection::sessionBus().isConnected());
        QTemporaryDir directory;Backend backend(directory.path());QTRY_VERIFY(!backend.busy());
        QDBusInterface properties("org.mpris.MediaPlayer2.plexfreq","/org/mpris/MediaPlayer2","org.freedesktop.DBus.Properties",QDBusConnection::sessionBus());
        QVERIFY(properties.isValid());
        QDBusPendingCallWatcher reply(properties.asyncCall("GetAll","org.mpris.MediaPlayer2.Player"));
        QTRY_VERIFY(reply.isFinished());QVERIFY(!reply.isError());
        const auto values=qdbus_cast<QVariantMap>(reply.reply().arguments().first());
        QCOMPARE(values.value("PlaybackStatus").toString(),QString("Stopped"));
        QCOMPARE(values.value("Position").toLongLong(),qint64(0));
        QVERIFY(values.value("CanControl").toBool());QVERIFY(!values.value("CanPlay").toBool());
        QDBusInterface player("org.mpris.MediaPlayer2.plexfreq","/org/mpris/MediaPlayer2","org.mpris.MediaPlayer2.Player",QDBusConnection::sessionBus());
        QDBusPendingCallWatcher pause(player.asyncCall("Pause"));QTRY_VERIFY(pause.isFinished());QVERIFY(!pause.isError());
    }
    void lyricsRepliesFollowCurrentTrackOnly() {
        NavigationBackend backend;QQmlEngine engine;engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/tests/Session.qml"));QVERIFY(component.isReady());
        std::unique_ptr<QObject> session(component.create());QVERIFY(session.get());
        backend.data.insert("track",QVariantMap{{"ratingKey","3"}});emit backend.stateChanged();
        session->setProperty("nowPlaying",true);QCOMPARE(backend.operation,QString("lyrics"));
        emit backend.completed("lyrics",true,QVariantMap{{"lyricsKey","3"},{"lyrics",QVariantList{QVariantMap{{"time",1000},{"text","First"}},QVariantMap{{"time",3000},{"text","Next"}}}}});
        QCOMPARE(session->property("lyrics").toList().size(),2);
        QVariant active;QVERIFY(QMetaObject::invokeMethod(session.get(),"lyricIndex",Q_RETURN_ARG(QVariant,active)));QCOMPARE(active.toInt(),0);
        backend.data.insert("track",QVariantMap{{"ratingKey","4"}});emit backend.stateChanged();
        QCOMPARE(session->property("lyrics").toList().size(),0);
        emit backend.completed("lyrics",true,QVariantMap{{"lyricsKey","3"},{"lyrics",QVariantList{QVariantMap{{"time",1000},{"text","Stale"}}}}});
        QCOMPARE(session->property("lyrics").toList().size(),0);
        emit backend.completed("lyrics",false,QVariantMap{{"lyricsKey","3"}});
        QCOMPARE(session->property("lyricsState").toString(),QString("loading"));
        emit backend.completed("lyrics",false,QVariantMap{{"lyricsKey","4"}});
        QCOMPARE(session->property("lyricsState").toString(),QString("unavailable"));
    }
    void failedLyricsRepliesKeepTheirRequestKey() {
        QTemporaryDir state;Backend backend(state.path());QTRY_VERIFY(!backend.busy());QSignalSpy completed(&backend,&Backend::completed);
        backend.command("lyrics",{{"key","999"}});
        bool found=false;QElapsedTimer deadline;deadline.start();while(!found && deadline.elapsed()<5000){QTest::qWait(25);for(const auto &reply:completed)if(reply[0].toString()=="lyrics"){QVERIFY(!reply[1].toBool());QCOMPARE(reply[2].toMap().value("lyricsKey").toString(),QString("999"));found=true;}}
        QVERIFY(found);
    }
    void savedArtistAlphabetJump() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");
        if (directory.isEmpty()) QSKIP("Live alphabet check is opt-in, metadata only");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free); QVERIFY(core.get());
        auto call=[&](const QVariantMap &request) { const auto input=QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact); char *response=pf_call(core.get(),input.constData()); const auto result=QJsonDocument::fromJson(QByteArray(response)).object(); pf_string_free(response); return result; };
        const auto libraries=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray(); QVERIFY(!libraries.isEmpty());
        const auto section=libraries.first().toObject().value("key").toString();
        const auto index=call({{"op","alphabet"},{"section",section}}); QVERIFY2(index.value("ok").toBool(),qPrintable(index.value("error").toString()));
        const auto groups=index.value("data").toObject().value("alphabet").toArray(); QVERIFY(groups.size()>1);
        const auto group=groups.at(groups.size()/2).toObject();
        const auto jump=call({{"op","jump_artist"},{"section",section},{"letter",group.value("letter").toString()}});
        QVERIFY2(jump.value("ok").toBool(),qPrintable(jump.value("error").toString()));
        const auto data=jump.value("data").toObject(); QVERIFY(data.value("replaceItems").toBool());
        QCOMPARE(data.value("start").toInt(),0); QCOMPARE(data.value("selectedIndex"),group.value("offset")); QVERIFY(!data.value("items").toArray().isEmpty());
        qDebug("Artist alphabet: %d groups; jump offset %d; page rows %d",int(groups.size()),data.value("selectedIndex").toInt(),int(data.value("items").toArray().size()));
    }
    void incrementalModelsPreserveExistingRows() {
        EntryModel model;
        const QVariant a=QVariantMap{{"ratingKey","1"},{"type","artist"},{"title","A"}};
        const QVariant b=QVariantMap{{"ratingKey","2"},{"type","artist"},{"title","B"}};
        const QVariant c=QVariantMap{{"ratingKey","3"},{"type","artist"},{"title","C"}};
        model.replace({a,b});
        QSignalSpy reset(&model,&QAbstractItemModel::modelReset);
        QSignalSpy inserted(&model,&QAbstractItemModel::rowsInserted);
        model.append({c}); QCOMPARE(model.rowCount(),3); QCOMPARE(reset.size(),0); QCOMPARE(inserted.size(),1);
        QCOMPARE(model.data(model.index(0),Qt::UserRole+1),a);
        model.replace({c,a,b}); QCOMPARE(reset.size(),0); QCOMPARE(model.rowCount(),3);
        QCOMPARE(model.data(model.index(1),Qt::UserRole+1),a);
    }
    void transparentPaginationIsDeduplicatedAndSearchIsDebounced() {
        NavigationBackend backend; QQmlEngine engine; engine.rootContext()->setContextProperty("backend",&backend);
        QQmlComponent component(&engine,QUrl("qrc:/tests/Session.qml")); QVERIFY(component.isReady());
        std::unique_ptr<QObject> session(component.create()); QVERIFY(session.get()); session->setProperty("section","1");
        QVERIFY(QMetaObject::invokeMethod(session.get(),"browse"));
        backend.data.insert("hasMore",true); backend.data.insert("next",100); emit backend.stateChanged();
        const int count=backend.commandCount;
        QVERIFY(QMetaObject::invokeMethod(session.get(),"maybeMore",Q_ARG(QVariant,QVariant(0.74)))); QCOMPARE(backend.commandCount,count);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"maybeMore",Q_ARG(QVariant,QVariant(0.75)))); QCOMPARE(backend.arguments.value("start").toInt(),100);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"maybeMore",Q_ARG(QVariant,QVariant(0.9)))); QCOMPARE(backend.commandCount,count+1);
        emit backend.completed("browse",false,QVariantMap{{"_pageStart",100}});
        QVERIFY(QMetaObject::invokeMethod(session.get(),"maybeMore",Q_ARG(QVariant,QVariant(0.9)))); QCOMPARE(backend.commandCount,count+1);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"search",Q_ARG(QVariant,QVariant("Fixture"))));
        QVERIFY(QMetaObject::invokeMethod(session.get(),"search",Q_ARG(QVariant,QVariant("Fixture final"))));
        QTRY_COMPARE(backend.arguments.value("query").toString(),QString("Fixture final")); QCOMPARE(backend.operation,QString("search"));
    }
    void passiveCachePollDoesNotPulseBusyOrRebindState() {
        QTemporaryDir state; Backend backend(state.path());
        QTRY_VERIFY(!backend.busy());
        QSignalSpy busy(&backend, &Backend::busyChanged);
        QSignalSpy changed(&backend, &Backend::stateChanged);
        QSignalSpy cache(&backend, &Backend::cacheChanged);
        QSignalSpy complete(&backend, &Backend::completed);
        backend.command("cache_status");
        QTRY_VERIFY(complete.size() > 0);
        QCOMPARE(busy.size(), 0);
        QCOMPARE(changed.size(), 0);
        QCOMPARE(cache.size(), 0);
    }
    void savedCacheDownload() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");
        if (directory.isEmpty()) QSKIP("Opt-in real media check, limited to one track up to 8 MiB");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new(directory.constData()),pf_free); QVERIFY(core.get());
        auto call=[&](const QVariantMap &request) {
            const auto input=QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact);
            char *response=pf_call(core.get(),input.constData()); const auto envelope=QJsonDocument::fromJson(QByteArray(response)).object(); pf_string_free(response); return envelope;
        };
        const auto libraries=call({{"op","libraries"}}).value("data").toObject().value("libraries").toArray(); QVERIFY(!libraries.isEmpty());
        const auto page=call({{"op","browse"},{"kind","track"},{"section",libraries.first().toObject().value("key").toString()},{"start",0}});
        QVERIFY2(page.value("ok").toBool(),qPrintable(page.value("error").toString()));
        QJsonObject selected;
        for (const auto &value:page.value("data").toObject().value("items").toArray()) {
            const auto item=value.toObject(); const auto media=item.value("Media").toArray(); if (media.isEmpty()) continue;
            const auto parts=media.first().toObject().value("Part").toArray(); if (parts.isEmpty()) continue;
            const auto size=parts.first().toObject().value("size").toDouble();
            if (size>0 && size<=8*1024*1024) { selected=item; break; }
        }
        if (selected.isEmpty()) QSKIP("No small downloadable sample in the first track page");
        const auto scheduled=call({{"op","cache_tracks"},{"items",QVariantList{selected.toVariantMap()}}});
        QVERIFY2(scheduled.value("ok").toBool(),qPrintable(scheduled.value("error").toString()));
        QElapsedTimer deadline; deadline.start();
        bool ready=false;
        while (deadline.elapsed()<60000) {
            const auto status=call({{"op","cache_status"}}).value("data").toObject().value("cache").toObject();
            QVERIFY2(status.value("error").toString().isEmpty(),qPrintable(status.value("error").toString()));
            if (status.value("readyKeys").toArray().contains(selected.value("ratingKey"))) { ready=true; break; }
            QTest::qWait(100);
        }
        QVERIFY2(ready,"Bounded live download did not finish within 60 seconds");
        const auto plan=call({{"op","play"},{"items",QVariantList{selected.toVariantMap()}},{"index",0}});
        QVERIFY2(plan.value("ok").toBool(),qPrintable(plan.value("error").toString()));
        const auto data=plan.value("data").toObject(); QCOMPARE(data.value("playbackSource").toString(),QString("cache"));
        const auto path=QUrl(data.value("stream").toString()).toLocalFile(); QVERIFY(!path.isEmpty()); QVERIFY(QFileInfo(path).size()>0);
        qDebug("Real audio cached bytes: %lld; local-file plan verified without playing it", static_cast<long long>(QFileInfo(path).size()));
    }
    void radioIconRemainsCompleteAcrossResizeAndReuse() {
        QQmlEngine engine;
        QQmlComponent component(&engine, QUrl("qrc:/tests/RadioIconGeometry.qml"));
        QVERIFY(component.isReady());
        QQuickWindow window; window.resize(160,160); window.setColor(Qt::black);
        std::unique_ptr<QObject> root(component.create());
        QVERIFY(root.get());
        auto *rootItem = qobject_cast<QQuickItem *>(root.get());
        QVERIFY(rootItem); rootItem->setParentItem(window.contentItem()); window.show();
        QVERIFY(QTest::qWaitForWindowExposed(&window));
        auto *icon = root->findChild<QQuickItem *>("radioIcon");
        QVERIFY(icon);
        for (const int size : {32, 96, 24, 72, 40, 32}) {
            icon->setVisible(false); icon->setWidth(size); icon->setHeight(size); icon->setVisible(true);
            auto image = icon->grabToImage();
            QVERIFY(image); QSignalSpy ready(image.data(), &QQuickItemGrabResult::ready);
            QTRY_VERIFY(!image->image().isNull());
            const auto pixels = image->image();
            int left = pixels.width(), right = -1, top = pixels.height(), bottom = -1;
            for (int y=0;y<pixels.height();++y) for (int x=0;x<pixels.width();++x) {
                if (qAlpha(pixels.pixel(x,y)) > 80 && qRed(pixels.pixel(x,y)) > 80) { left=qMin(left,x); right=qMax(right,x); top=qMin(top,y); bottom=qMax(bottom,y); }
            }
            QVERIFY(right-left > pixels.width()*0.55);
            QVERIFY(bottom-top > pixels.height()*0.65);
            QVERIFY(bottom > pixels.height()*0.8); // mast base, not only top-left waves
            QVERIFY(left > 0 && right < pixels.width()-1);
        }
    }
    void detailHeaderReservesSpaceForRows() {
        QQmlEngine engine;
        QQmlComponent component(&engine, QUrl("qrc:/tests/HeaderGeometry.qml"));
        if (!component.isReady()) qWarning() << component.errors();
        QVERIFY(component.isReady());
        QQuickWindow window;
        window.resize(480, 1000);
        std::unique_ptr<QObject> root(component.create());
        QVERIFY(root.get());
        auto *rootItem = qobject_cast<QQuickItem *>(root.get());
        QVERIFY(rootItem);
        rootItem->setParentItem(window.contentItem());
        window.show();
        QVERIFY(QTest::qWaitForWindowExposed(&window));
        auto *loader = root->findChild<QQuickItem *>("headerLoader");
        auto *content = root->findChild<QQuickItem *>("detailContent");
        auto *row = root->findChild<QQuickItem *>("firstRow");
        QVERIFY(loader && content && row);
        QTRY_VERIFY(content->implicitHeight() > 280);
        QTRY_COMPARE(loader->height(), content->implicitHeight());
        QTRY_VERIFY(row->y() >= loader->y() + content->implicitHeight());
        const qreal wideHeight = loader->height();
        root->setProperty("width", 240);
        QTRY_VERIFY(loader->height() > wideHeight);
        QTRY_VERIFY(row->y() >= loader->y() + content->implicitHeight());
        loader->setProperty("active", false);
        QTRY_COMPARE(loader->height(), qreal(0));
        loader->setProperty("active", true);
        content = root->findChild<QQuickItem *>("detailContent");
        QVERIFY(content);
        QTRY_COMPARE(loader->height(), content->implicitHeight());
        QTRY_VERIFY(row->y() >= loader->y() + content->implicitHeight());
    }
    void artistAlbumNavigationAndRadioSeed() {
        NavigationBackend backend;
        QQmlEngine engine;
        engine.rootContext()->setContextProperty("backend", &backend);
        QQmlComponent component(&engine, QUrl("qrc:/tests/Session.qml"));
        if (!component.isReady()) qWarning() << component.errors();
        QVERIFY(component.isReady());
        std::unique_ptr<QObject> session(component.create());
        QVERIFY(session.get());
        session->setProperty("section", "1");
        session->setProperty("query", "Fixture search");
        QVERIFY(QMetaObject::invokeMethod(session.get(), "browse"));
        QCOMPARE(backend.operation, QString("browse"));
        QCOMPARE(backend.arguments.value("kind").toString(), QString("artist"));
        QSignalSpy navigate(session.get(), SIGNAL(navigate(QString,QString)));
        const QVariant artist = QVariantMap{{"ratingKey", "10"}, {"type", "artist"}, {"title", "Artist"}};
        const QVariant album = QVariantMap{{"ratingKey", "20"}, {"type", "album"}, {"title", "Album"}};
        QVERIFY(QMetaObject::invokeMethod(session.get(), "activate", Q_ARG(QVariant, artist), Q_ARG(QVariant, QVariant(0))));
        QCOMPARE(backend.operation, QString("detail"));
        QCOMPARE(backend.arguments.value("key").toString(), QString("10"));
        QCOMPARE(navigate.size(), 1);
        backend.data.insert("detail", QVariantMap{{"ratingKey", "10"}, {"type", "artist"}, {"summary", "Biography"}});
        emit backend.stateChanged();
        emit backend.completed("detail",true,QVariantMap{{"detail",backend.data.value("detail")},{"start",0}});
        QCOMPARE(backend.operation,QString("similar_artists"));
        const QVariant relatedArtist=QVariantMap{{"ratingKey","11"},{"type","artist"},{"title","Related artist"}};
        emit backend.completed("similar_artists",true,QVariantMap{{"similarKey","10"},{"similarAvailable",true},{"similarArtists",QVariantList{relatedArtist}}});
        QCOMPARE(session->property("similarArtists").toList().size(),1);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"activate",Q_ARG(QVariant,relatedArtist),Q_ARG(QVariant,QVariant(0))));
        QCOMPARE(backend.arguments.value("key").toString(),QString("11"));
        QCOMPARE(session->property("similarArtists").toList().size(),0);
        emit backend.completed("similar_artists",true,QVariantMap{{"similarKey","10"},{"similarAvailable",true},{"similarArtists",QVariantList{relatedArtist}}});
        QCOMPARE(session->property("similarArtists").toList().size(),0);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"back"));
        QCOMPARE(backend.arguments.value("key").toString(),QString("10"));
        navigate.clear();
        QVERIFY(QMetaObject::invokeMethod(session.get(), "activate", Q_ARG(QVariant, album), Q_ARG(QVariant, QVariant(0))));
        QCOMPARE(backend.arguments.value("key").toString(), QString("20"));
        QCOMPARE(navigate.size(), 1);
        QVERIFY(QMetaObject::invokeMethod(session.get(), "back"));
        QCOMPARE(backend.operation, QString("detail"));
        QCOMPARE(backend.arguments.value("key").toString(), QString("10"));
        QVERIFY(QMetaObject::invokeMethod(session.get(), "startRadio", Q_ARG(QVariant, artist)));
        QCOMPARE(backend.operation, QString("radio"));
        QCOMPARE(backend.arguments.value("kind").toString(), QString("artist"));
        QVERIFY(QMetaObject::invokeMethod(session.get(), "back"));
        QCOMPARE(backend.operation, QString("browse"));
        QCOMPARE(backend.arguments.value("kind").toString(), QString("artist"));
        QCOMPARE(backend.arguments.value("query").toString(), QString("Fixture search"));
        QVERIFY(!session->property("canGoBack").toBool());
#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
        // Instantiate the actual detail header with fixture metadata, including
        // its icon action, rather than only loading an empty library window.
        QVERIFY(QMetaObject::invokeMethod(session.get(), "activate", Q_ARG(QVariant, artist), Q_ARG(QVariant, QVariant(0))));
        backend.data.insert("detail", QVariantMap{{"ratingKey", "10"}, {"type", "artist"}, {"title", "Artist"}, {"summary", "Biography"}, {"photos", QVariantList()}});
        emit backend.stateChanged();
        emit backend.completed("detail",true,QVariantMap{{"detail",backend.data.value("detail")},{"start",0}});
        QQmlComponent headerComponent(&engine, QUrl("qrc:/qml/desktop/DetailHeader.qml"));
        if (!headerComponent.isReady()) qWarning() << headerComponent.errors();
        QVERIFY(headerComponent.isReady());
        QQuickWindow detailWindow; detailWindow.resize(600,800);
        std::unique_ptr<QObject> header(headerComponent.beginCreate(engine.rootContext()));
        QVERIFY(header.get());
        header->setProperty("music", QVariant::fromValue(session.get()));
        header->setProperty("width", 600);
        headerComponent.completeCreate();
        auto *headerItem=qobject_cast<QQuickItem *>(header.get()); QVERIFY(headerItem);
        headerItem->setParentItem(detailWindow.contentItem()); detailWindow.show(); QVERIFY(QTest::qWaitForWindowExposed(&detailWindow));
        QVERIFY(!headerComponent.isError());
        QTRY_VERIFY(header->property("implicitHeight").toDouble() > 0);
        QVERIFY(QMetaObject::invokeMethod(session.get(),"activate",Q_ARG(QVariant,album),Q_ARG(QVariant,QVariant(0))));
        emit backend.completed("detail",true,QVariantMap{{"detail",QVariantMap{{"ratingKey","20"},{"type","album"},{"title","Album"},{"summary","Description"},{"photos",QVariantList()}}},{"start",0}});
        header->setProperty("width",320);
        auto *actions=header->findChild<QQuickItem *>("albumActions");
        auto *play=header->findChild<QQuickItem *>("playTracks"); auto *download=header->findChild<QQuickItem *>("downloadTracks");
        QVERIFY(actions && play && download);
        // This component is tested with actual controls; widths are bounded by
        // the action row even at the narrow layout used on mobile-sized windows.
        QTRY_VERIFY(actions->width()>0);
        QTRY_VERIFY(play->x()+play->width()<=actions->width()+1);
        QTRY_VERIFY(download->x()+download->width()<=actions->width()+1);
#endif
    }
    void savedLibraryDetails() {
        const auto directory = qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");
        if (directory.isEmpty()) QSKIP("Live detail metadata check is opt-in");
        std::unique_ptr<Core, decltype(&pf_free)> core(pf_new_inspect(directory.constData()), pf_free);
        QVERIFY(core.get());
        auto call = [&](const QVariantMap &request) {
            const auto input = QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact);
            char *response = pf_call(core.get(), input.constData());
            const auto envelope = QJsonDocument::fromJson(QByteArray(response)).object();
            pf_string_free(response);
            return envelope;
        };
        const auto libraries = call({{"op", "libraries"}}).value("data").toObject().value("libraries").toArray();
        QVERIFY(!libraries.isEmpty());
        const auto browse = call({{"op", "browse"}, {"kind", "artist"}, {"section", libraries.first().toObject().value("key").toString()}, {"start", 0}});
        QVERIFY(browse.value("ok").toBool());
        const auto artists = browse.value("data").toObject().value("items").toArray();
        QVERIFY(!artists.isEmpty());
        const auto artist = call({{"op", "detail"}, {"key", artists.first().toObject().value("ratingKey").toString()}, {"start", 0}});
        QVERIFY2(artist.value("ok").toBool(), qPrintable(artist.value("error").toString()));
        const auto artistData = artist.value("data").toObject();
        QCOMPARE(artistData.value("detail").toObject().value("type").toString(), QString("artist"));
        const auto albums = artistData.value("items").toArray();
        QVERIFY(!albums.isEmpty());
        qDebug("Artist detail: album count %d, photo count %d, biography present %d", int(albums.size()), int(artistData.value("detail").toObject().value("photos").toArray().size()), !artistData.value("detail").toObject().value("summary").toString().isEmpty());
        const auto similar=call({{"op","similar_artists"},{"key",artists.first().toObject().value("ratingKey").toString()}});
        QVERIFY2(similar.value("ok").toBool(),qPrintable(similar.value("error").toString()));
        qDebug("Similar artists: count %d, endpoint available %d",int(similar.value("data").toObject().value("similarArtists").toArray().size()),similar.value("data").toObject().value("similarAvailable").toBool());
        const auto album = call({{"op", "detail"}, {"key", albums.first().toObject().value("ratingKey").toString()}, {"start", 0}});
        QVERIFY2(album.value("ok").toBool(), qPrintable(album.value("error").toString()));
        const auto albumData = album.value("data").toObject();
        QCOMPARE(albumData.value("detail").toObject().value("type").toString(), QString("album"));
        QVERIFY(!albumData.value("items").toArray().isEmpty());
        qDebug("Album detail: track count %d, cover count %d, description present %d", int(albumData.value("items").toArray().size()), int(albumData.value("detail").toObject().value("photos").toArray().size()), !albumData.value("detail").toObject().value("summary").toString().isEmpty());
    }
    void savedDiscoveryViewsAndLyrics() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Metadata-only discovery check is opt-in");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &request){const auto input=QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),input.constData());const auto result=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return result;};
        const auto libraryReply=call({{"op","libraries"}});QVERIFY2(libraryReply.value("ok").toBool(),qPrintable(libraryReply.value("error").toString()));
        const auto libraries=libraryReply.value("data").toObject().value("libraries").toArray();QVERIFY(!libraries.isEmpty());const auto section=libraries.first().toObject().value("key").toString();
        const auto artists=call({{"op","browse"},{"section",section},{"kind","artist"},{"start",0}}).value("data").toObject().value("items").toArray();QVERIFY(!artists.isEmpty());
        const auto search=call({{"op","search"},{"query",artists.first().toObject().value("title").toString()}});QVERIFY2(search.value("ok").toBool(),qPrintable(search.value("error").toString()));
        const auto results=search.value("data").toObject().value("items").toArray();QVERIFY(!results.isEmpty());
        for(const auto &item:results){const auto type=item.toObject().value("type").toString();QVERIFY(type=="artist" || type=="album" || type=="track");}
        qDebug("Global music search returned %d items",int(results.size()));
        for(const auto &view:QStringList{"favorites","added","played"}){
            const auto reply=call({{"op","collection"},{"section",section},{"view",view},{"start",0}});QVERIFY2(reply.value("ok").toBool(),qPrintable(reply.value("error").toString()));
            qDebug("Discovery view %s: %d rows",qPrintable(view),int(reply.value("data").toObject().value("items").toArray().size()));
        }
        const auto tracks=call({{"op","browse"},{"section",section},{"kind","track"},{"start",0}}).value("data").toObject().value("items").toArray();QVERIFY(!tracks.isEmpty());
        const auto lyrics=call({{"op","lyrics"},{"key",tracks.first().toObject().value("ratingKey").toString()}});
        qDebug("Sample lyrics request available %d; lines %d",lyrics.value("ok").toBool(),int(lyrics.value("data").toObject().value("lyrics").toArray().size()));
    }
    void savedCurrentLyricsAndPlaylistStats() {
        const auto directory=qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");if(directory.isEmpty())QSKIP("Read-only current-lyric/playlist check is opt-in");
        std::unique_ptr<Core,decltype(&pf_free)> core(pf_new_inspect(directory.constData()),pf_free);QVERIFY(core.get());
        auto call=[&](const QVariantMap &request){const auto input=QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact);char *response=pf_call(core.get(),input.constData());const auto result=QJsonDocument::fromJson(QByteArray(response)).object();pf_string_free(response);return result;};
        const auto playlists=call({{"op","playlists"},{"start",0}});QVERIFY2(playlists.value("ok").toBool(),qPrintable(playlists.value("error").toString()));
        const auto items=playlists.value("data").toObject().value("items").toArray();int counts=0,durations=0;
        for(const auto &value:items){const auto item=value.toObject();if(!item.value("leafCount").isNull())++counts;if(!item.value("totalDuration").isNull())++durations;}
        qDebug("Playlist summaries: %d rows; %d counts; %d total durations",int(items.size()),counts,durations);
        const auto status=call({{"op","status"}});QVERIFY(status.value("ok").toBool());const auto track=status.value("data").toObject().value("track").toObject();
        if(track.isEmpty()){qDebug("No saved current track for lyric validation");return;}
        const auto lyrics=call({{"op","lyrics"},{"key",track.value("ratingKey").toString()}});QVERIFY2(lyrics.value("ok").toBool(),qPrintable(lyrics.value("error").toString()));
        const auto lines=lyrics.value("data").toObject().value("lyrics").toArray();
        for(const auto &value:lines)QVERIFY(!value.toObject().value("text").toString().contains("\"MediaContainer\""));
        qDebug("Current track decoded lyric lines: %d",int(lines.size()));
    }
    void savedRadioPlanning() {
        const auto directory = qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR");
        if (directory.isEmpty()) QSKIP("Live radio planning is opt-in; no music is fetched or played");
        QVERIFY(QFileInfo(QString::fromUtf8(directory) + "/session.json").exists());
        std::unique_ptr<Core, decltype(&pf_free)> core(pf_new_inspect(directory.constData()), pf_free);
        QVERIFY(core.get());
        auto call = [&](const QVariantMap &request) {
            const auto input = QJsonDocument::fromVariant(request).toJson(QJsonDocument::Compact);
            char *response = pf_call(core.get(), input.constData());
            const auto envelope = QJsonDocument::fromJson(QByteArray(response)).object();
            pf_string_free(response);
            return envelope;
        };
        const auto libraries = call({{"op", "libraries"}}).value("data").toObject().value("libraries").toArray();
        QVERIFY(!libraries.isEmpty());
        const auto section = libraries.first().toObject().value("key").toString();
        for (const auto &kind : {QString("artist"), QString("album"), QString("track")}) {
            const auto page = call({{"op", "browse"}, {"section", section}, {"kind", kind}, {"start", 0}});
            QVERIFY2(page.value("ok").toBool(), qPrintable(page.value("error").toString()));
            const auto items = page.value("data").toObject().value("items").toArray();
            if (items.isEmpty()) continue;
            const auto result = call({{"op", "radio"}, {"key", items.first().toObject().value("ratingKey").toString()}, {"kind", kind}});
            if (result.value("ok").toBool()) {
                const int count = static_cast<int>(result.value("data").toObject().value("queue").toObject().value("items").toArray().size());
                QVERIFY(count > 0);
                qDebug("%s radio planned track count: %d", qPrintable(kind), count);
                if (kind == "artist" && count <= 50) {
                    QJsonObject continued;
                    for (int i = 0; i < count; ++i) {
                        continued = call({{"op", "next"}, {"automatic", true}});
                        QVERIFY2(continued.value("ok").toBool(), qPrintable(continued.value("error").toString()));
                    }
                    const int expanded = static_cast<int>(continued.value("data").toObject().value("queue").toObject().value("items").toArray().size());
                    QVERIFY(expanded > count);
                    qDebug("Artist radio continued metadata count: %d", expanded);
                }
            } else {
                const auto error = result.value("error").toString();
                QVERIFY2(kind != "artist", qPrintable(error));
                QVERIFY(error.contains("sonic recommendations") || error.contains("sonically similar"));
                qDebug("%s radio unavailable for sampled seed (sonic analysis)", qPrintable(kind));
            }
        }
    }
    void savedAccountDiscovery() {
        const auto directory = QString::fromUtf8(qgetenv("PLEXFREQ_ACCOUNT_CHECK_STATE_DIR"));
        if (directory.isEmpty()) QSKIP("Live account check is opt-in; local fixture is exercised separately");
        QVERIFY(QFileInfo(directory + "/session.json").exists());
        Backend backend(directory);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        QVERIFY(backend.state().value("signedIn").toBool());
        backend.command("servers");
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 20000);
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
        const int count = backend.state().value("servers").toList().size();
        QVERIFY2(count > 0, "The saved account returned no usable servers");
        qDebug("Discovered server count: %d", count); // Never print response/credentials/URLs.
    }
    void protocolAndNativeAudio() {
        QTemporaryDir state;
        QVERIFY(state.isValid());
        QTcpServer server;
        QVERIFY(server.listen(QHostAddress::LocalHost));
        bool tokenSeen = false;
        QByteArray wave;
        QDataStream wav(&wave, QIODevice::WriteOnly);
        wav.setByteOrder(QDataStream::LittleEndian);
        const int samples = 8000 * 10;
        wav.writeRawData("RIFF", 4); wav << quint32(36 + samples * 2);
        wav.writeRawData("WAVEfmt ", 8); wav << quint32(16) << quint16(1) << quint16(1);
        wav << quint32(8000) << quint32(16000) << quint16(2) << quint16(16);
        wav.writeRawData("data", 4); wav << quint32(samples * 2);
        wave.append(QByteArray(samples * 2, '\0')); // Generated local silence, no external media.
        connect(&server, &QTcpServer::newConnection, this, [&] {
            while (server.hasPendingConnections()) {
                auto *socket = server.nextPendingConnection();
                connect(socket, &QTcpSocket::disconnected, socket, &QObject::deleteLater);
                connect(socket, &QTcpSocket::readyRead, this, [&, socket] {
                    QByteArray request = socket->property("request").toByteArray() + socket->readAll();
                    socket->setProperty("request", request);
                    if (!request.contains("\r\n\r\n")) return;
                    QByteArray body;
                    QByteArray contentType = "application/json";
                    QByteArray status = "200 OK";
                    QByteArray extraHeaders;
                    const auto target = request.split(' ').value(1);
                    const auto path = QUrl(QString::fromUtf8("http://fixture" + target)).path();
                    if (path == "/library/sections") {
                        tokenSeen = request.toLower().contains("x-plex-token: test-token");
                        body = R"({"MediaContainer":{"Directory":[{"key":"1","title":"Local fixture","type":"artist"}]}})";
                    } else if (path == "/library/sections/1/all") {
                        body = R"({"MediaContainer":{"size":2,"totalSize":2,"Metadata":[{"ratingKey":"1","key":"/library/metadata/1","title":"First","type":"track","duration":10000,"Media":[{"Part":[{"key":"/library/parts/1/silence.wav"}]}]},{"ratingKey":"2","key":"/library/metadata/2","title":"Second","type":"track","duration":10000,"Media":[{"Part":[{"key":"/library/parts/2/silence.wav"}]}]}]}})";
                    } else if (path.startsWith("/library/parts/")) {
                        body = wave; contentType = "audio/wav";
                        extraHeaders = "Accept-Ranges: bytes\r\n";
                        const auto range = QRegularExpression("range: bytes=(\\d+)-(\\d*)", QRegularExpression::CaseInsensitiveOption).match(QString::fromUtf8(request));
                        if (range.hasMatch()) {
                            const int start = range.captured(1).toInt();
                            const int end = range.captured(2).isEmpty() ? wave.size() - 1 : qMin(range.captured(2).toInt(), int(wave.size()) - 1);
                            body = wave.mid(start, end - start + 1);
                            status = "206 Partial Content";
                            extraHeaders += "Content-Range: bytes " + QByteArray::number(start) + "-" + QByteArray::number(end) + "/" + QByteArray::number(wave.size()) + "\r\n";
                        }
                    } else {
                        body = R"({"MediaContainer":{}})";
                    }
                    socket->write("HTTP/1.1 " + status + "\r\nContent-Type: " + contentType +
                        "\r\n" + extraHeaders + "Content-Length: " + QByteArray::number(body.size()) + "\r\nConnection: close\r\n\r\n" + body);
                    socket->disconnectFromHost();
                });
            }
        });
        Backend backend(state.path());
        QSignalSpy completed(&backend, &Backend::completed);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        backend.command("connect", {{"url", QString("http://127.0.0.1:%1").arg(server.serverPort())}, {"token", "test-token"}});
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
        QVERIFY(tokenSeen);
        QCOMPARE(backend.state().value("libraries").toList().size(), 1);
        backend.command("browse", {{"section", "1"}, {"kind", "track"}, {"start", 0}});
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        const auto items = backend.state().value("items").toList();
        QCOMPARE(items.size(), 2);
        backend.command("play", {{"items", items}, {"index", 0}});
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        QTRY_VERIFY_WITH_TIMEOUT(backend.duration() == 10000, 10000);
        if(!qgetenv("PLEXFREQ_MEDIA_ROLE_CHECK").isEmpty()) {
            QTRY_VERIFY(backend.playing());
            QDBusInterface lookup("org.PulseAudio1","/org/pulseaudio/server_lookup1","org.PulseAudio.ServerLookup1",QDBusConnection::sessionBus());
            const auto address=lookup.property("Address").toString();QVERIFY(!address.isEmpty());
            {auto peer=QDBusConnection::connectToPeer(address,"plexfreq-volume-fixture");QVERIFY(peer.isConnected());
                QDBusInterface properties(QString(),"/com/meego/mainvolume2","org.freedesktop.DBus.Properties",peer);
                QString media;QElapsedTimer deadline;deadline.start();
                while(media!="background" && media!="active" && media!="foreground" && deadline.elapsed()<3000){
                    QDBusPendingCallWatcher reply(properties.asyncCall("Get","com.Meego.MainVolume2","MediaState"));QTRY_VERIFY(reply.isFinished());QVERIFY(!reply.isError());
                    media=reply.reply().arguments().first().value<QDBusVariant>().variant().toString();QTest::qWait(25);
                }
                QVERIFY2(media=="background" || media=="active" || media=="foreground",qPrintable("Native media state: "+media));
            }QDBusConnection::disconnectFromPeer("plexfreq-volume-fixture");
        }
        QTRY_VERIFY_WITH_TIMEOUT(backend.playing(), 5000);
        QTRY_VERIFY_WITH_TIMEOUT(backend.position() >= 100, 5000);
        QCOMPARE(backend.state().value("track").toMap().value("title").toString(), QString("First"));
        QVERIFY(!backend.state().contains("stream"));
        if(!qgetenv("PLEXFREQ_RECORDING_CHECK").isEmpty()) {
            // The phone's existing silent null source exercises recording policy
            // without microphone samples or changes to PulseAudio configuration.
            QProcess recording;recording.setStandardOutputFile(QProcess::nullDevice());
            recording.start("parec",QStringList{"--device=source.null","--raw"});
            QVERIFY(recording.waitForStarted(3000));
            QTRY_VERIFY_WITH_TIMEOUT(backend.paused() && !backend.playing(),3000);
            const auto pausedPosition=backend.position();
            recording.terminate();QVERIFY(recording.waitForFinished(3000));
            QTRY_VERIFY_WITH_TIMEOUT(backend.playing(),3000);
            QVERIFY(backend.position()>=pausedPosition);
            // An explicit pause while recording must cancel automatic resume.
            recording.start("parec",QStringList{"--device=source.null","--raw"});
            QVERIFY(recording.waitForStarted(3000));
            QTRY_VERIFY_WITH_TIMEOUT(backend.paused(),3000);
            backend.pause();QTest::qWait(200);
            recording.terminate();QVERIFY(recording.waitForFinished(3000));
            QTest::qWait(600);QVERIFY(backend.paused());QVERIFY(!backend.playing());
            backend.play();QTRY_VERIFY_WITH_TIMEOUT(backend.playing(),3000);
        }
        backend.pause();
        QTRY_VERIFY(!backend.playing());
        QTRY_VERIFY(backend.paused());
        QSignalSpy seeked(&backend,&Backend::seeked);
        backend.seek(2000);
        QTRY_VERIFY(backend.position() >= 1900);
        QTRY_VERIFY(seeked.size()>0);
        backend.play();
        QTRY_VERIFY(backend.playing());
        QTRY_VERIFY_WITH_TIMEOUT(backend.position() >= 2200, 5000);
        QTRY_VERIFY_WITH_TIMEOUT(backend.cache().value("tracks").toInt() >= 2, 10000);
        backend.command("offline_mode",{{"enabled",true}});QTRY_VERIFY(!backend.busy());
        QTRY_VERIFY_WITH_TIMEOUT(backend.position()>=7300,8000);
        backend.pause();QTRY_VERIFY(!backend.playing());
        QTRY_COMPARE(backend.state().value("history").toMap().value("pending").toInt(),1);
        server.close(); // no new HTTP requests can succeed from this point
        backend.command("next", {{"automatic", false}});
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        QCOMPARE(backend.state().value("track").toMap().value("title").toString(), QString("Second"));
        QCOMPARE(backend.state().value("playbackSource").toString(), QString("cache"));
        QTRY_VERIFY_WITH_TIMEOUT(backend.duration() == 10000 && backend.playing(), 5000);
        backend.seek(9900);
        QTRY_VERIFY_WITH_TIMEOUT(backend.state().value("queue").toMap().value("current").isNull(), 5000);
        QTRY_VERIFY(!backend.playing());
        QCOMPARE(backend.state().value("history").toMap().value("pending").toInt(),1); // seeking the second track to its end is not another listen
        backend.command("select_track",{{"index",1}});QTRY_VERIFY(!backend.busy());QTRY_VERIFY(backend.playing());
        backend.command("logout");
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy(), 5000);
        QTRY_VERIFY(!backend.playing());
        QVERIFY(backend.state().value("serverUrl").toString().isEmpty());
        QVERIFY(backend.state().value("queue").toMap().value("items").toList().isEmpty());
        backend.play();QVERIFY(!backend.playing());
        QVERIFY(completed.size() >= 6);
    }
};
QTEST_MAIN(BackendTest)
#include "qt_backend.moc"
