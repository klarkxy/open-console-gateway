; Identity migration after an in-place replace of a pre-rename install.
; Lifecycle (skip reinstall page, restore INSTDIR, wipe dist, data checkbox)
; lives in windows/installer.nsi.

!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $0 SHCTX "${LEGACYMANUPRODUCTKEY}" ""
  ${If} $0 == $INSTDIR
    DeleteRegKey SHCTX "${LEGACYMANUPRODUCTKEY}"
    DeleteRegKey SHCTX "${LEGACYUNINSTKEY}"
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${LEGACYPRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      ${IfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
        Rename "$SMPROGRAMS\${LEGACYPRODUCTNAME}.lnk" "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      ${Else}
        !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        Pop $0
        ${If} $0 = 1
          Delete "$SMPROGRAMS\${LEGACYPRODUCTNAME}.lnk"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    !insertmacro IsShortcutTarget "$DESKTOP\${LEGACYPRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      ${IfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
        Rename "$DESKTOP\${LEGACYPRODUCTNAME}.lnk" "$DESKTOP\${PRODUCTNAME}.lnk"
      ${Else}
        !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        Pop $0
        ${If} $0 = 1
          Delete "$DESKTOP\${LEGACYPRODUCTNAME}.lnk"
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend
