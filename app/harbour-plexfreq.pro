TARGET = harbour-plexfreq
VERSION = 0.1.6
# sailfishapp would install the whole qml/ tree, including the desktop
# QtQuick.Controls 2 sources; ship only the Silica entry point and its imports.
CONFIG += sailfishapp sailfishapp_no_deploy_qml c++11
CONFIG += link_pkgconfig
PKGCONFIG += libpulse
DEFINES += SAILFISH PLEXFREQ_VERSION=\\\"$$VERSION\\\"
SOURCES += src/main.cpp src/backend.cpp src/i18n.cpp
HEADERS += src/backend.h src/plexfreq_core.h src/i18n.h
RESOURCES += translations.qrc icons.qrc
catalogues.files = translations/*.qm
catalogues.path = /usr/share/harbour-plexfreq/translations
INSTALLS += catalogues
qml.files = qml/harbour-plexfreq.qml qml/pages qml/shared
qml.path = /usr/share/$${TARGET}/qml
INSTALLS += qml
LIBS += $$PWD/thirdparty/libplexfreq_core.a -lpthread -ldl -lm
LIBS += -lsailfishapp -lmdeclarativecache5
QMAKE_LFLAGS += -pie
PRE_TARGETDEPS += $$PWD/thirdparty/libplexfreq_core.a
DISTFILES += harbour-plexfreq.desktop rpm/harbour-plexfreq.spec qml/harbour-plexfreq.qml qml/pages/*.qml qml/shared/*.qml
# Launcher icons in every size Harbour validation expects.
for(size, $$list(86 108 128 172)) {
    icon$${size}.files = icons/$${size}x$${size}/harbour-plexfreq.png
    icon$${size}.path = /usr/share/icons/hicolor/$${size}x$${size}/apps
    INSTALLS += icon$${size}
}
