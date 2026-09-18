/**
 * The one locale this window formats dates in.
 *
 * Letting `toLocaleDateString` choose a locale asks the machine, which can
 * make an English shell render Portuguese dates. The interface copy is English and
 * assumes day-first dates, so use British English everywhere.
 */
export const UI_LOCALE = "en-GB";
