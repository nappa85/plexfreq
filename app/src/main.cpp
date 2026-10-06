#include "backend.h"
#include "i18n.h"
#include <QGuiApplication>
#include <QQmlContext>
#include <QStandardPaths>
#include <QTimer>
#include <QDateTime>
#include <QFile>
#include <QFileInfo>
#include <QIcon>
#include <QWindow>
#include <QDir>
#include <qqml.h>
#include <cstdio>
#include <memory>
#ifdef SAILFISH
#include <sailfishapp.h>
#include <QQuickView>
#include <fcntl.h>
#include <unistd.h>
#else
#include <QQmlApplicationEngine>
#endif

#ifdef SAILFISH
static void installPersistentLogging() {
    if (!qgetenv("PLEXFREQ_STATE_DIR").isEmpty()) return;
    const QString documents = QStandardPaths::writableLocation(QStandardPaths::DocumentsLocation);
    if (documents.isEmpty()) return;
    QString directory = QDir(documents).absoluteFilePath(QStringLiteral("PlexFreq"));
    if (!QDir().mkpath(directory)) {
        // Documents is not writable under the app's existing Sailjail permissions.
        directory = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation)
            + QStringLiteral("/logs");
        if (!QDir().mkpath(directory)) return;
    }
    QFile::setPermissions(directory, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
    const QString path = QDir(directory).absoluteFilePath(
        QStringLiteral("plexfreq-%1.log").arg(QDate::currentDate().toString(QStringLiteral("yyyy-MM-dd"))));
    const QByteArray nativePath = QFile::encodeName(path);
    const int descriptor = ::open(nativePath.constData(), O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (descriptor < 0) return;
    ::dup2(descriptor, STDOUT_FILENO);
    ::dup2(descriptor, STDERR_FILENO);
    ::close(descriptor);
    std::fprintf(stderr, "\n=== PlexFreq %s started ===\n", qPrintable(QDateTime::currentDateTime().toString(Qt::ISODate)));
    std::fflush(stderr);
}
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
    application->setWindowIcon(QIcon(QStringLiteral(":/icons/plexfreq.png")));
#ifdef SAILFISH
    installPersistentLogging();
#endif
    qmlRegisterType<EntryModel>("PlexFreq",1,0,"EntryModel");
    installTranslations(application);
    QString directory = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    // Isolated state for local integration tests; never touches the user's login.
    if (!qgetenv("PLEXFREQ_STATE_DIR").isEmpty()) directory = QString::fromUtf8(qgetenv("PLEXFREQ_STATE_DIR"));
    if (directory.isEmpty()) directory = QDir::temp().absoluteFilePath(QStringLiteral("plexfreq"));
    Backend backend(directory);
    QObject::connect(&backend,&Backend::raiseRequested,application,[]{for(auto *window:QGuiApplication::topLevelWindows()){window->show();window->raise();window->requestActivate();}});
#ifdef SAILFISH
    QQuickView *view = SailfishApp::createView();
    view->rootContext()->setContextProperty("backend", &backend);
    const QString bundledQml = application->applicationDirPath() + "/qml/harbour-plexfreq.qml";
    view->setSource(QFileInfo(bundledQml).exists() ? QUrl::fromLocalFile(bundledQml)
        : SailfishApp::pathTo("qml/harbour-plexfreq.qml"));
    if (view->status() == QQuickView::Error) {delete view;return 1;}
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
