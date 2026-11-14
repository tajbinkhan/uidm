// The only code that reaches the shell (AGENTS.md section 7.4): the window's
// subscription, and the commands as plain async functions. Each throws an
// InvokeError whose message is already in the user's language.
import { Channel } from '@tauri-apps/api/core';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { commands } from '../bindings';
import type {
  AnswerInput,
  ColumnKey,
  DownloadControl,
  Id,
  InvokeError,
  LimitInput,
  ListAction,
  OnDoneInput,
  OpenHow,
  OptionsDraft,
  PickKind,
  QueueDraft,
  QuotaDraft,
  GrabDraft,
  GrabScheduleDraft,
  FilterInput,
  BatchInput,
  CategoryInput,
  ColumnChoice,
  ExportFormat,
  ExportScope,
  FindInput,
  LimiterDraft,
  PropertiesInput,
  OpenWindow,
  PickInput,
  LinkFilter,
  ToolKey,
  DebridChoice,
  ReportAction,
  ConvertTarget,
  ExceptionChoice,
  Update,
  SeedLimitsInput,
} from './types';
import { receive } from '../stores/projection.svelte';
import { zoom } from '../theme';

export type * from './types';

/** Each subscription's number; updates on an older one are dropped. */
let generation = 0;

/** Subscribe this window to its view. A gap in the numbers subscribes again,
 * which brings a fresh snapshot. */
export async function connect(): Promise<void> {
  const mine = ++generation;
  let expected = 0;
  const channel = new Channel<Update>();
  channel.onmessage = (update) => {
    if (mine !== generation) return;
    if (update.seq !== expected) {
      void connect();
      return;
    }
    expected = update.seq + 1;
    receive(update);
  };
  await commands.subscribe(channel, [...navigator.languages]);
}

/** Send unexpected errors to the shell's log (AGENTS.md section 11). */
export function reportErrors(): void {
  const report = (message: string) => {
    void commands.reportFrontendError(message).catch(() => undefined);
  };
  window.addEventListener('error', (e) => report(`${e.message} at ${e.filename}:${e.lineno}`));
  window.addEventListener('unhandledrejection', (e) => report(String(e.reason?.stack ?? e.reason)));
}

/** The message of a failed command, for the dialog to show. */
export function messageOf(e: unknown): string {
  const err = e as Partial<InvokeError> | undefined;
  return typeof err?.message === 'string' ? err.message : String(e);
}

export const closeWindow = () => commands.closeWindow();
export const openWindow = (w: OpenWindow) => commands.openWindow(w);
export const selectRows = (ids: Id[]) => commands.selectRows(ids);
export const selectNode = (key: string) => commands.selectNode(key);
export const toggleNode = (key: string) => commands.toggleNode(key);
export const sortList = (column: ColumnKey) => commands.sortList(column);
export const resizeColumn = (column: ColumnKey, width: number) => commands.resizeColumn(column, Math.round(width));
export const toggleCategories = () => commands.toggleCategories();
export const runListAction = (action: ListAction) => commands.runListAction(action);
export const openDownload = (id: Id, how: OpenHow) => commands.openDownload(id, how);
export const showDownload = (id: Id) => commands.showDownload(id);
export const addDownload = (address: string, login: { user: string; password: string } | null) =>
  commands.addDownload(address, login);
export const answerDownload = (answer: AnswerInput) => commands.answerDownload(answer);
export const checkSaveFolder = (saveAs: string, folderOnly = false) => commands.checkSaveFolder(saveAs, folderOnly);
export const pickSaveFolder = (saveAs: string, folderOnly = false) => commands.pickSaveFolder(saveAs, folderOnly);
export const removeDownloads = (deleteFiles: boolean, dontAsk: boolean) => commands.removeDownloads(deleteFiles, dontAsk);
export const hideDoneDialogs = () => commands.hideDoneDialogs();
export const controlDownload = (action: DownloadControl) => commands.controlDownload(action);
export const setDownloadLimit = (limit: LimitInput) => commands.setDownloadLimit(limit);
export const setDownloadOnDone = (onDone: OnDoneInput) => commands.setDownloadOnDone(onDone);

/** Fit the window to its content's height, as the progress window does when
 * its details open and close. */
export async function fitHeight(width: number, height: number): Promise<void> {
  await getCurrentWindow().setSize(new LogicalSize(width * zoom(), height * zoom()));
}
export const quitApp = () => commands.quitApp();
export const saveOptions = (draft: OptionsDraft) => commands.saveOptions(draft);
export const pickPath = (kind: PickKind, start: string) => commands.pickPath(kind, start);
export const saveQueue = (draft: QueueDraft) => commands.saveQueue(draft);
export const createQueue = (name: string) => commands.createQueue(name);
export const deleteQueue = (id: Id) => commands.deleteQueue(id);
export const controlQueue = (id: Id, start: boolean) => commands.controlQueue(id, start);
export const moveQueued = (id: Id, offset: number) => commands.moveQueued(id, offset);
export const removeQueued = (id: Id) => commands.removeQueued(id);
export const saveQuota = (quota: QuotaDraft) => commands.saveQuota(quota);
export const grabStart = (draft: GrabDraft) => commands.grabStart(draft);
export const grabStop = () => commands.grabStop();
export const grabSelect = (key: string) => commands.grabSelect(key);
export const grabNew = () => commands.grabNew();
export const grabAdd = (indices: number[], queue: Id, start: boolean) => commands.grabAdd(indices, queue, start);
export const grabTemplate = (name: string) => commands.grabTemplate(name);
export const grabOpen = (name: string) => commands.grabOpen(name);
export const grabSave = (name: string, draft: GrabDraft, checked: boolean[], schedule: GrabScheduleDraft) =>
  commands.grabSave(name, draft, checked, schedule);
export const grabSaveTemplate = (name: string, draft: GrabDraft) => commands.grabSaveTemplate(name, draft);
export const grabSaveFilters = (filters: FilterInput[]) => commands.grabSaveFilters(filters);
export const grabAddOnClose = (on: boolean) => commands.grabAddOnClose(on);

/** Resize this window, as the grabber does between its wizard and results. */
export async function setWindowSize(width: number, height: number): Promise<void> {
  await getCurrentWindow().setSize(new LogicalSize(width * zoom(), height * zoom()));
}

/** This window's title, such as the grabber's step. */
export async function setWindowTitle(title: string): Promise<void> {
  await getCurrentWindow().setTitle(title);
}
export const previewBatch = (input: BatchInput) => commands.previewBatch(input);
export const addBatch = (input: BatchInput, login: { user: string; password: string } | null) => commands.addBatch(input, login);
export const openClipboardLinks = (text: string) => commands.openClipboardLinks(text);
export const addLinks = (indices: number[], queue: Id, start: boolean) => commands.addLinks(indices, queue, start);
export const importList = () => commands.importList();
export const exportList = (scope: ExportScope, format: ExportFormat) => commands.exportList(scope, format);
export const findDownload = (input: FindInput) => commands.findDownload(input);
export const showProperties = (id: Id) => commands.showProperties(id);
export const saveProperties = (input: PropertiesInput) => commands.saveProperties(input);
export const moveDownload = (id: Id, name: string) => commands.moveDownload(id, name);
export const setLimiter = (draft: LimiterDraft) => commands.setLimiter(draft);
export const setColumns = (columns: ColumnChoice[]) => commands.setColumns(columns);
export const saveCategory = (original: string | null, input: CategoryInput | null) => commands.saveCategory(original, input);
export const browseCategory = (name: string) => commands.browseCategory(name);
export const queueSelection = (queue: Id | null) => commands.queueSelection(queue);
export const showDropTarget = (on: boolean) => commands.showDropTarget(on);

/** The clipboard's text, read on the user's click. */
export async function clipboardText(): Promise<string> {
  return navigator.clipboard.readText();
}

/** Move the frameless drop target with the mouse. */
export async function dragWindow(): Promise<void> {
  await getCurrentWindow().startDragging();
}
export const setPanelWidth = (width: number) => commands.setPanelWidth(Math.round(width));
export const setListFont = (font: { mono: boolean; size: number } | null) => commands.setListFont(font);
export const setToolbar = (small: boolean, buttons: string[]) => commands.setToolbar(small, buttons);
export const closeTip = (next: number, onStartup: boolean) => commands.closeTip(next, onStartup);
export const answerLogin = (user: string, password: string, save: boolean) => commands.answerLogin(user, password, save);
export const answerCountdown = (now: boolean) => commands.answerCountdown(now);
export const showRefresh = (id: Id) => commands.showRefresh(id);
export const refreshLink = (id: Id, url: string | null, restart: boolean) => commands.refreshLink(id, url, restart);
export const cancelRefresh = () => commands.cancelRefresh();
export const unpackPackage = (id: Id, password: string | null) => commands.unpackPackage(id, password);
export const askPackagePassword = (id: Id) => commands.askPackagePassword(id);
export const ungroupPackage = (id: Id) => commands.ungroupPackage(id);
export const openPackage = (id: Id) => commands.openPackage(id);
export const addPicks = (input: PickInput) => commands.addPicks(input);
export const setLinkFilter = (filter: LinkFilter) => commands.setLinkFilter(filter);
export const linkNames = (indices: number[], rename: string) => commands.linkNames(indices, rename);
export const checkUpdate = () => commands.checkUpdate();
export const installUpdate = () => commands.installUpdate();
export const dismissRelease = () => commands.dismissRelease();
export const installTool = (tool: ToolKey) => commands.installTool(tool);
export const checkDebrid = (service: DebridChoice, key: string) => commands.checkDebrid(service, key);
export const remoteStatus = () => commands.remoteStatus();
export const pairPhone = (on: boolean) => commands.pairPhone(on);
export const removeDevice = (id: Id) => commands.removeDevice(id);
export const setBrowser = (index: number, on: boolean) => commands.setBrowser(index, on);
export const addBrowser = (program: string) => commands.addBrowser(program);
export const removeBrowser = (index: number) => commands.removeBrowser(index);
export const restartBrowser = (index: number) => commands.restartBrowser(index);
export const openThemesFolder = () => commands.openThemesFolder();
export const playSound = (index: number, file: string) => commands.playSound(index, file);
export const systemProxy = () => commands.systemProxy();
export const heartbeat = (visible: boolean) => commands.heartbeat(visible);
export const reportAction = (action: ReportAction) => commands.reportAction(action);
export const previewZip = (id: Id) => commands.previewZip(id);
export const convertDownload = (id: Id, target: ConvertTarget, deleteOriginal: boolean) => commands.convertDownload(id, target, deleteOriginal);
export const verifyChecksum = (id: Id, checksum: string) => commands.verifyChecksum(id, checksum);
export const openWith = (id: Id) => commands.openWith(id);
export const dragFile = (id: Id) => commands.dragFile(id);
export const answerException = (choice: ExceptionChoice, dontShow: boolean) => commands.answerException(choice, dontShow);
export const pickTorrentFiles = (indexes: number[], chosen: boolean) => commands.pickTorrentFiles(indexes, chosen);
export const setSeedLimits = (limits: SeedLimitsInput) => commands.setSeedLimits(limits);
export const stopSeeding = () => commands.stopSeeding();
export const openTorrentFile = () => commands.openTorrentFile();
