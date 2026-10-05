#include "i18n.h"
#include <QTranslator>
#include <QLocale>
#include <QDir>
#include <QRegularExpression>
#include <QSettings>

static QString stripQuotes(const QString &value) {
    QString v=value.trimmed();
    if(v.size()>=2 && ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith('\'') && v.endsWith('\''))))v=v.mid(1,v.size()-2);
    return v.trimmed();
}
QString translationLocale(const QString &overrideLocale, const QString &systemLocale, const QString &fallbackConfig) {
    QString locale=stripQuotes(overrideLocale);
    if(locale.isEmpty()) {
        if(!fallbackConfig.isEmpty()) {
            QSettings config(fallbackConfig,QSettings::IniFormat);
            locale=stripQuotes(config.value("LC_MESSAGES").toString());
            if(locale.isEmpty())locale=stripQuotes(config.value("LANG").toString());
        }
        if(locale.isEmpty())locale=stripQuotes(systemLocale);
    }
    locale=locale.section('.',0,0).section('@',0,0);locale.replace('-','_');
    return locale;
}

void installTranslations(QCoreApplication *app) {
    QString fallbackConfig;
#ifdef SAILFISH
    // Developer SSH launches may omit the locale inherited by normal GUI apps.
    if(qgetenv("LC_ALL").isEmpty() && qgetenv("LC_MESSAGES").isEmpty() && qgetenv("LANG").isEmpty() && qgetenv("LANGUAGE").isEmpty())fallbackConfig="/etc/locale.conf";
#endif
    const QString locale=translationLocale(QString::fromUtf8(qgetenv("PLEXFREQ_LANGUAGE")),QLocale::system().name(),fallbackConfig);
    // Install base first, then territory, so missing territory keys fall back
    // to the base language instead of English. Keep every loaded translator
    // alive via the app parent; install all that exist.
    QStringList tags;if(locale.contains('_'))tags<<locale.section('_',0,0);tags<<locale;
    const QString binary=QCoreApplication::applicationDirPath();
    const QStringList paths{binary+"/translations",QDir(binary).absoluteFilePath("../share/plexfreq/translations"),"/usr/share/harbour-plexfreq/translations",":/translations"};
    for(const auto &tag:tags)for(const auto &path:paths){auto *translator=new QTranslator(app);if(translator->load("harbour-plexfreq_"+tag,path)){app->installTranslator(translator);}else{delete translator;}}
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
