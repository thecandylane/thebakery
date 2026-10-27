// Core editor contributions (find, bracket matching, multicursor, comment,
// snippet, folding, hover, suggest UI, ...) — deliberately NOT
// language-service "features" (TS/JSON/CSS/HTML validation, each of which
// wants its own worker; see monacoSetup.ts's doc comment). This list is
// `esm/vs/editor/editor.main.js`'s own contrib import block from
// monaco-editor@0.56, minus its `languages/features/*` and
// `languages/definitions/*` imports (handled separately) — every entry here
// is a pure editor-UI behavior that runs through the single `editor.worker`
// already wired, if it needs a worker at all. Side-effect only.
//
// The two `codicons/codicon*.css` entries from that source list are omitted:
// `monaco-editor`'s package.json `exports` map only exposes `.js` subpaths
// to outside consumers (its own internal `.js` files can still reach their
// sibling `.css` via relative imports — that's how `editor.api.js` still
// gets its own base stylesheet). Icon glyphs for some contrib gutter/hover
// affordances render as missing glyphs without them; nothing here is
// functionally gated on them.
import "monaco-editor/editor/contrib/anchorSelect/browser/anchorSelect.js";
import "monaco-editor/editor/contrib/bracketMatching/browser/bracketMatching.js";
import "monaco-editor/editor/contrib/caretOperations/browser/transpose.js";
import "monaco-editor/editor/contrib/clipboard/browser/clipboard.js";
import "monaco-editor/editor/contrib/codeAction/browser/codeActionContributions.js";
import "monaco-editor/editor/browser/widget/codeEditor/codeEditorWidget.js";
import "monaco-editor/editor/contrib/codelens/browser/codelensController.js";
import "monaco-editor/editor/contrib/colorPicker/browser/colorPickerContribution.js";
import "monaco-editor/editor/contrib/comment/browser/comment.js";
import "monaco-editor/editor/contrib/contextmenu/browser/contextmenu.js";
import "monaco-editor/editor/contrib/cursorUndo/browser/cursorUndo.js";
import "monaco-editor/editor/browser/widget/diffEditor/diffEditor.contribution.js";
import "monaco-editor/editor/contrib/diffEditorBreadcrumbs/browser/contribution.js";
import "monaco-editor/editor/contrib/dnd/browser/dnd.js";
import "monaco-editor/editor/contrib/documentSymbols/browser/documentSymbols.js";
import "monaco-editor/editor/contrib/dropOrPasteInto/browser/dropIntoEditorContribution.js";
import "monaco-editor/features/find/register.js";
import "monaco-editor/editor/contrib/floatingMenu/browser/floatingMenu.contribution.js";
import "monaco-editor/editor/contrib/folding/browser/folding.js";
import "monaco-editor/editor/contrib/fontZoom/browser/fontZoom.js";
import "monaco-editor/editor/contrib/format/browser/formatActions.js";
import "monaco-editor/editor/contrib/gotoError/browser/gotoError.js";
import "monaco-editor/editor/standalone/browser/quickAccess/standaloneGotoLineQuickAccess.js";
import "monaco-editor/editor/contrib/gotoSymbol/browser/link/goToDefinitionAtPosition.js";
import "monaco-editor/editor/contrib/gpu/browser/gpuActions.js";
import "monaco-editor/editor/contrib/hover/browser/hoverContribution.js";
import "monaco-editor/editor/contrib/indentation/browser/indentation.js";
import "monaco-editor/editor/contrib/inlayHints/browser/inlayHintsContribution.js";
import "monaco-editor/editor/contrib/inlineCompletions/browser/inlineCompletions.contribution.js";
import "monaco-editor/editor/contrib/inlineProgress/browser/inlineProgress.js";
import "monaco-editor/editor/contrib/inPlaceReplace/browser/inPlaceReplace.js";
import "monaco-editor/editor/contrib/insertFinalNewLine/browser/insertFinalNewLine.js";
import "monaco-editor/editor/standalone/browser/inspectTokens/inspectTokens.js";
import "monaco-editor/editor/standalone/browser/iPadShowKeyboard/iPadShowKeyboard.js";
import "monaco-editor/editor/contrib/lineSelection/browser/lineSelection.js";
import "monaco-editor/editor/contrib/linesOperations/browser/linesOperations.js";
import "monaco-editor/editor/contrib/linkedEditing/browser/linkedEditing.js";
import "monaco-editor/editor/contrib/links/browser/links.js";
import "monaco-editor/editor/contrib/longLinesHelper/browser/longLinesHelper.js";
import "monaco-editor/editor/contrib/middleScroll/browser/middleScroll.contribution.js";
import "monaco-editor/editor/contrib/multicursor/browser/multicursor.js";
import "monaco-editor/editor/contrib/parameterHints/browser/parameterHints.js";
import "monaco-editor/editor/contrib/placeholderText/browser/placeholderText.contribution.js";
import "monaco-editor/editor/standalone/browser/quickAccess/standaloneCommandsQuickAccess.js";
import "monaco-editor/editor/standalone/browser/quickAccess/standaloneHelpQuickAccess.js";
import "monaco-editor/editor/standalone/browser/quickAccess/standaloneGotoSymbolQuickAccess.js";
import "monaco-editor/editor/contrib/readOnlyMessage/browser/contribution.js";
import "monaco-editor/editor/standalone/browser/referenceSearch/standaloneReferenceSearch.js";
import "monaco-editor/editor/contrib/rename/browser/rename.js";
import "monaco-editor/editor/contrib/sectionHeaders/browser/sectionHeaders.js";
import "monaco-editor/editor/contrib/semanticTokens/browser/viewportSemanticTokens.js";
import "monaco-editor/editor/contrib/smartSelect/browser/smartSelect.js";
import "monaco-editor/editor/contrib/snippet/browser/snippetController2.js";
import "monaco-editor/editor/contrib/stickyScroll/browser/stickyScrollContribution.js";
import "monaco-editor/editor/contrib/suggest/browser/suggestInlineCompletions.js";
import "monaco-editor/editor/standalone/browser/toggleHighContrast/toggleHighContrast.js";
import "monaco-editor/editor/contrib/toggleTabFocusMode/browser/toggleTabFocusMode.js";
import "monaco-editor/editor/contrib/tokenization/browser/tokenization.js";
import "monaco-editor/editor/contrib/unicodeHighlighter/browser/unicodeHighlighter.js";
import "monaco-editor/editor/contrib/unusualLineTerminators/browser/unusualLineTerminators.js";
import "monaco-editor/editor/contrib/wordHighlighter/browser/wordHighlighter.js";
import "monaco-editor/editor/contrib/wordOperations/browser/wordOperations.js";
import "monaco-editor/editor/contrib/wordPartOperations/browser/wordPartOperations.js";
import "monaco-editor/editor/browser/coreCommands.js";
import "monaco-editor/editor/contrib/caretOperations/browser/caretOperations.js";
import "monaco-editor/editor/contrib/dropOrPasteInto/browser/copyPasteContribution.js";
import "monaco-editor/editor/contrib/find/browser/findController.js";
import "monaco-editor/editor/contrib/gotoSymbol/browser/goToCommands.js";
import "monaco-editor/editor/contrib/gotoError/browser/markerSelectionStatus.js";
import "monaco-editor/editor/contrib/semanticTokens/browser/documentSemanticTokens.js";
import "monaco-editor/editor/contrib/suggest/browser/suggestController.js";
import "monaco-editor/editor/common/standaloneStrings.js";
