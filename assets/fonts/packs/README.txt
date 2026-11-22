Language font packs
===================

DontreeCrab embeds Sarabun for Thai and Latin (which also covers Vietnamese).
The additional scripts below ship as font files so the executable stays small
and the packs can be updated independently.

  NotoSansJP-Regular.otf  Japanese      Noto Sans JP (subset OTF)
  NotoSansKR-Regular.otf  Korean        Noto Sans KR (subset OTF)
  NotoSansSC-Regular.otf  Chinese       Noto Sans SC (subset OTF, simplified)
  NotoSansLao.ttf         Lao           Noto Sans Lao (variable TTF)

All four are licensed under the SIL Open Font License 1.1:

  LICENSE-NotoSansCJK.txt   copyright Google / Adobe, SIL OFL 1.1
  LICENSE-NotoSansLao.txt   copyright Google, SIL OFL 1.1

Sources
-------

  https://github.com/notofonts/noto-cjk  (Sans/SubsetOTF)
  https://github.com/google/fonts        (ofl/notosanslao)

The player loads the packs at runtime from this folder next to the executable
(`assets/fonts/packs`). If a pack is missing the lyrics fall back to Sarabun,
so a build without these files still runs; the missing script shows as empty
boxes.

The fonts are unmodified. If you replace one, keep the OFL license file and
do not use a Reserved Font Name for a modified version.
