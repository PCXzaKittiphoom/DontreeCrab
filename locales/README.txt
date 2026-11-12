DontreeCrab translations
=======================

English (en.po) and Thai (th.po) are built into the program. Additional
languages can be added without rebuilding it.

Adding a language
-----------------

1. Copy dontreecrab.pot to a new file named after the language code, for
   example fr.po or de.po.
2. Edit the header:

       "Language: fr\n"
       "Language-Name: Français\n"

3. Translate every msgstr line. Keep the {} placeholders; they may be moved
   but not removed.
4. Save the file as UTF-8.
5. Restart DontreeCrab and choose the language in
   Settings > General > Language.

Where to put the file
---------------------

The player reads .po files from:

  * the locales folder next to DontreeCrab.exe
  * a locales folder in the working directory
  * %APPDATA%\DontreeCrab\DontreeCrab\config\locales

A file with the same language code as a built-in catalog overrides it.
Missing entries fall back to English, so a partial translation is fine.
