#include "i18n.h"
#include <QTranslator>
#include <QLocale>
#include <QDir>
#include <QRegularExpression>

void installTranslations(QCoreApplication *app) {
    QString locale=QString::fromUtf8(qgetenv("PLEXFREQ_LANGUAGE"));if(locale.isEmpty())locale=QLocale::system().name();locale.replace('-','_');
    QStringList tags{locale};if(locale.contains('_'))tags<<locale.section('_',0,0);
    const QString binary=QCoreApplication::applicationDirPath();
    const QStringList paths{binary+"/translations",QDir(binary).absoluteFilePath("../share/plexfreq/translations"),"/usr/share/harbour-plexfreq/translations",":/translations"};
    for(const auto &tag:tags)for(const auto &path:paths){auto *translator=new QTranslator(app);if(translator->load("harbour-plexfreq_"+tag,path)){app->installTranslator(translator);return;}delete translator;}
}
QString translatedMessage(const QString &source) {
    if(source.isEmpty())return source;
    const auto http=QRegularExpression("^Plex returned HTTP ([0-9]+)$").match(source);
    if(http.hasMatch())return QCoreApplication::translate("Core","Plex returned HTTP %1").arg(http.captured(1));
    const QString prefix="Unexpected Plex response during ";
    if(source.startsWith(prefix))return QCoreApplication::translate("Core","Unexpected Plex response during %1").arg(QCoreApplication::translate("Core",source.mid(prefix.size()).toUtf8().constData()));
    return QCoreApplication::translate("Core",source.toUtf8().constData());
}
QString translatedError(const QVariantMap &response) {
    QString text=translatedMessage(response.value("errorSource",response.value("error")).toString());
    for(const auto &arg:response.value("errorArgs").toList())text=text.arg(translatedMessage(arg.toString()));
    return text;
}
void translateErrorFields(QVariantMap &data) {
    for(auto it=data.begin();it!=data.end();++it){
        if(it.key()=="error" && it.value().canConvert<QString>())it.value()=translatedMessage(it.value().toString());
        else if(it.key()=="errors"){auto map=it.value().toMap();for(auto e=map.begin();e!=map.end();++e)e.value()=translatedMessage(e.value().toString());it.value()=map;}
        else if(it.value().userType()==QMetaType::QVariantMap){auto map=it.value().toMap();translateErrorFields(map);it.value()=map;}
        else if(it.value().userType()==QMetaType::QVariantList){auto list=it.value().toList();for(auto &value:list)if(value.userType()==QMetaType::QVariantMap){auto map=value.toMap();translateErrorFields(map);value=map;}it.value()=list;}
    }
}
