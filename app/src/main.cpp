#include "backend.h"
#include "i18n.h"
#include <QGuiApplication>
#include <QQmlContext>
#include <QStandardPaths>
#include <QTimer>
#include <QFileInfo>
#include <QWindow>
#include <memory>
#ifdef SAILFISH
#include <sailfishapp.h>
#include <QQuickView>
#else
#include <QQmlApplicationEngine>
#endif

int main(int argc, char *argv[]) {
#ifdef SAILFISH
    QGuiApplication *application = SailfishApp::application(argc, argv);
#else
    QGuiApplication *application = new QGuiApplication(argc, argv);
#endif
    std::unique_ptr<QGuiApplication> applicationOwner(application);
    application->setOrganizationName("org.plexfreq");
    application->setApplicationName("harbour-plexfreq");
    application->setApplicationVersion("0.1.0");
    installTranslations(application);
    QString directory = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    // Isolated state for local integration tests; never touches the user's login.
    if (!qgetenv("PLEXFREQ_STATE_DIR").isEmpty()) directory = QString::fromUtf8(qgetenv("PLEXFREQ_STATE_DIR"));
    Backend backend(directory);
    QObject::connect(&backend,&Backend::raiseRequested,application,[]{for(auto *window:QGuiApplication::topLevelWindows()){window->show();window->raise();window->requestActivate();}});
#ifdef SAILFISH
    QQuickView *view = SailfishApp::createView();
    view->rootContext()->setContextProperty("backend", &backend);
    const QString bundledQml = application->applicationDirPath() + "/qml/harbour-plexfreq.qml";
    view->setSource(QFileInfo(bundledQml).exists() ? QUrl::fromLocalFile(bundledQml)
        : SailfishApp::pathTo("qml/harbour-plexfreq.qml"));
    if (view->status() == QQuickView::Error) return 1;
    view->showFullScreen();
#else
    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty("backend", &backend);
    engine.load(QUrl("qrc:/qml/desktop/Main.qml"));
    if (engine.rootObjects().isEmpty()) return 1;
#endif
    if (application->arguments().contains("--smoke-test")) QTimer::singleShot(1500, application, SLOT(quit()));
    const int result = application->exec();
#ifdef SAILFISH
    delete view;
#endif
    return result;
}
