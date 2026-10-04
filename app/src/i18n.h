#ifndef PLEXFREQ_I18N_H
#define PLEXFREQ_I18N_H
#include <QCoreApplication>
#include <QString>
#include <QVariantMap>
void installTranslations(QCoreApplication *application);
QString translatedError(const QVariantMap &response);
QString translatedMessage(const QString &source);
void translateErrorFields(QVariantMap &data);
#endif
