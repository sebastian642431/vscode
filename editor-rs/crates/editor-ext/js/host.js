'use strict';
// editor-rs extension host bootstrap.
//
// Runs in Node.js, spawned by the Rust `editor_ext::host::ExtensionHost`. It:
//   1. speaks JSON-RPC (Content-Length framing) with Rust over stdio;
//   2. provides a `vscode` module shim, injected via a require() hook, backed
//      by requests to Rust for anything that needs the editor;
//   3. loads and activates CommonJS VS Code extensions, and runs the commands
//      they register.
//
// stdout carries ONLY protocol frames; the extension's console output is
// redirected to Rust as `log` notifications so it never corrupts the stream.

const Module = require('module');
const path = require('path');

// ---------------------------------------------------------------- transport
function send(obj) {
  const body = Buffer.from(JSON.stringify(obj), 'utf8');
  process.stdout.write('Content-Length: ' + body.length + '\r\n\r\n');
  process.stdout.write(body);
}

let nextId = 1;
const pendingToRust = new Map();

function requestRust(method, params) {
  return new Promise((resolve) => {
    const id = 'c' + nextId++;
    pendingToRust.set(id, resolve);
    send({ jsonrpc: '2.0', id: id, method: method, params: params || {} });
  });
}
function notifyRust(method, params) {
  send({ jsonrpc: '2.0', method: method, params: params || {} });
}

// Redirect extension logging off stdout.
for (const k of ['log', 'info', 'warn', 'error', 'debug', 'trace']) {
  console[k] = (...a) =>
    notifyRust('log', {
      message: a
        .map((x) => (typeof x === 'string' ? x : safeStringify(x)))
        .join(' '),
    });
}
function safeStringify(x) {
  try {
    return JSON.stringify(x);
  } catch (_) {
    return String(x);
  }
}

// ---------------------------------------------------------------- incoming
let inbuf = Buffer.alloc(0);
process.stdin.on('data', (chunk) => {
  inbuf = Buffer.concat([inbuf, chunk]);
  for (;;) {
    const headerEnd = inbuf.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const header = inbuf.slice(0, headerEnd).toString('utf8');
    const m = /content-length:\s*(\d+)/i.exec(header);
    if (!m) {
      inbuf = inbuf.slice(headerEnd + 4);
      continue;
    }
    const len = parseInt(m[1], 10);
    const start = headerEnd + 4;
    if (inbuf.length < start + len) return;
    const body = inbuf.slice(start, start + len).toString('utf8');
    inbuf = inbuf.slice(start + len);
    let msg;
    try {
      msg = JSON.parse(body);
    } catch (_) {
      continue;
    }
    onMessage(msg);
  }
});
process.stdin.on('end', () => process.exit(0));
process.stdin.resume();

async function onMessage(msg) {
  if (typeof msg.method === 'string') {
    const { id, method, params } = msg;
    if (id === undefined || id === null) {
      handleNotification(method, params);
      return;
    }
    try {
      const result = await handleRequest(method, params);
      send({ jsonrpc: '2.0', id: id, result: result === undefined ? null : result });
    } catch (e) {
      send({
        jsonrpc: '2.0',
        id: id,
        error: { code: -32000, message: String((e && e.stack) || e) },
      });
    }
  } else if (msg.id !== undefined && msg.id !== null) {
    const resolve = pendingToRust.get(msg.id);
    if (resolve) {
      pendingToRust.delete(msg.id);
      resolve(msg.result);
    }
  }
}

function handleNotification(method, params) {
  switch (method) {
    case 'shutdown':
      process.exit(0);
      break;
    case 'deactivate':
      deactivateAll();
      break;
    case 'document.open':
      documents.set(params.uri, new TextDocument(params.uri, params.languageId, params.version, params.text));
      break;
    case 'document.change': {
      const doc = documents.get(params.uri);
      if (doc) doc._reset(params.version, params.text);
      break;
    }
    case 'document.close':
      documents.delete(params.uri);
      if (activeEditorUri === params.uri) activeEditorUri = null;
      break;
    case 'editor.setActive':
      activeEditorUri = params.uri || null;
      break;
  }
}

async function handleRequest(method, params) {
  switch (method) {
    case 'activate':
      return activateExtension(params);
    case 'executeCommand':
      return runCommand(params.command, params.args || []);
    case 'provider.completion':
      return provideCompletion(params);
    case 'provider.hover':
      return provideHover(params);
    case 'provider.definition':
      return provideDefinition(params);
    case 'deactivate':
      deactivateAll();
      return null;
    default:
      return null;
  }
}

// --- invoking language-feature providers the extension registered ----------

function selectorMatches(selector, doc) {
  if (!selector) return true;
  const arr = Array.isArray(selector) ? selector : [selector];
  return arr.some((s) => {
    if (typeof s === 'string') return s === '*' || s === doc.languageId;
    if (s && typeof s === 'object') {
      return !s.language || s.language === '*' || s.language === doc.languageId;
    }
    return false;
  });
}

function newToken() {
  return {
    isCancellationRequested: false,
    onCancellationRequested: new EventEmitter().event,
  };
}

function mdToString(d) {
  if (d == null) return null;
  if (typeof d === 'string') return d;
  if (typeof d.value === 'string') return d.value;
  return null;
}

function normalizeCompletionItem(it) {
  if (typeof it === 'string') return { label: it, insertText: it };
  const label = typeof it.label === 'string' ? it.label : (it.label && it.label.label) || '';
  let insertText = it.insertText;
  let isSnippet = false;
  if (insertText && typeof insertText === 'object' && 'value' in insertText) {
    insertText = insertText.value;
    isSnippet = true;
  }
  return {
    label,
    kind: typeof it.kind === 'number' ? it.kind : null,
    detail: it.detail || null,
    documentation: mdToString(it.documentation),
    insertText: insertText != null ? String(insertText) : label,
    isSnippet,
    sortText: it.sortText || null,
    filterText: it.filterText || null,
  };
}

async function provideCompletion(params) {
  const doc = documents.get(params.uri);
  if (!doc) return [];
  const pos = new Position(params.line, params.character);
  const token = newToken();
  const ctx = { triggerKind: 1, triggerCharacter: params.triggerCharacter };
  const out = [];
  for (const { selector, provider } of providers.completion) {
    if (!selectorMatches(selector, doc)) continue;
    if (typeof provider.provideCompletionItems !== 'function') continue;
    let res = await Promise.resolve(provider.provideCompletionItems(doc, pos, token, ctx));
    if (!res) continue;
    const items = Array.isArray(res) ? res : res.items || [];
    for (const it of items) out.push(normalizeCompletionItem(it));
  }
  return out;
}

async function provideHover(params) {
  const doc = documents.get(params.uri);
  if (!doc) return null;
  const pos = new Position(params.line, params.character);
  const token = newToken();
  for (const { selector, provider } of providers.hover) {
    if (!selectorMatches(selector, doc)) continue;
    if (typeof provider.provideHover !== 'function') continue;
    const res = await Promise.resolve(provider.provideHover(doc, pos, token));
    if (!res) continue;
    const contents = (Array.isArray(res.contents) ? res.contents : [res.contents])
      .map(mdToString)
      .filter((x) => x != null);
    return { contents, range: res.range ? rangeEditOf(res.range, '') : null };
  }
  return null;
}

function normalizeLocation(loc) {
  if (!loc) return null;
  // Location | LocationLink
  const uri = loc.uri || loc.targetUri;
  const range = loc.range || loc.targetRange;
  if (!uri || !range) return null;
  return {
    uri: typeof uri === 'string' ? uri : uri.toString(),
    startLine: range.start.line,
    startCharacter: range.start.character,
    endLine: range.end.line,
    endCharacter: range.end.character,
  };
}

async function provideDefinition(params) {
  const doc = documents.get(params.uri);
  if (!doc) return [];
  const pos = new Position(params.line, params.character);
  const token = newToken();
  for (const { selector, provider } of providers.definition) {
    if (!selectorMatches(selector, doc)) continue;
    if (typeof provider.provideDefinition !== 'function') continue;
    const res = await Promise.resolve(provider.provideDefinition(doc, pos, token));
    if (!res) continue;
    const arr = Array.isArray(res) ? res : [res];
    return arr.map(normalizeLocation).filter((x) => x != null);
  }
  return [];
}

// ---------------------------------------------------------------- vscode shim
const commandMap = new Map(); // id -> { cb, thisArg }
const providers = { completion: [], hover: [], definition: [], codeAction: [] };
const activeExtensions = new Map(); // id -> { mod, ctx }
const documents = new Map(); // uri -> TextDocument
let activeEditorUri = null;

function makeDisposable(fn) {
  return { dispose: typeof fn === 'function' ? fn : () => {} };
}

class EventEmitter {
  constructor() {
    this._listeners = [];
  }
  get event() {
    return (listener, thisArgs, disposables) => {
      const fn = thisArgs ? listener.bind(thisArgs) : listener;
      this._listeners.push(fn);
      const d = makeDisposable(() => {
        this._listeners = this._listeners.filter((x) => x !== fn);
      });
      if (disposables && Array.isArray(disposables)) disposables.push(d);
      return d;
    };
  }
  fire(e) {
    for (const fn of this._listeners.slice()) fn(e);
  }
  dispose() {
    this._listeners = [];
  }
}

class Position {
  constructor(line, character) {
    this.line = line;
    this.character = character;
  }
  translate(dl, dc) {
    return new Position(this.line + (dl || 0), this.character + (dc || 0));
  }
  with(line, character) {
    return new Position(line == null ? this.line : line, character == null ? this.character : character);
  }
  isEqual(o) {
    return o && this.line === o.line && this.character === o.character;
  }
  isBefore(o) {
    return this.line < o.line || (this.line === o.line && this.character < o.character);
  }
}
class Range {
  constructor(start, end) {
    this.start = start;
    this.end = end;
  }
  get isEmpty() {
    return this.start.isEqual(this.end);
  }
  contains(p) {
    return !p.isBefore(this.start) && !this.end.isBefore(p);
  }
  with(start, end) {
    return new Range(start || this.start, end || this.end);
  }
}
class Selection extends Range {
  constructor(anchor, active) {
    super(anchor, active);
    this.anchor = anchor;
    this.active = active;
  }
}
class Location {
  constructor(uri, rangeOrPos) {
    this.uri = uri;
    this.range = rangeOrPos;
  }
}

// A mirror of a document the editor opened. Columns/offsets are UTF-16 code
// units (JS string indices), matching VS Code's API.
class TextDocument {
  constructor(uri, languageId, version, text) {
    this.uri = typeof uri === 'string' ? Uri.parse(uri) : uri;
    this._uriString = typeof uri === 'string' ? uri : uri.toString();
    this.languageId = languageId || 'plaintext';
    this.version = version || 0;
    this._reset(this.version, text || '');
  }
  _reset(version, text) {
    this.version = version;
    this._text = text;
    // Precompute line start offsets.
    this._lineStarts = [0];
    for (let i = 0; i < text.length; i++) {
      if (text.charCodeAt(i) === 10 /* \n */) this._lineStarts.push(i + 1);
    }
  }
  get fileName() {
    return this.uri.fsPath;
  }
  get isUntitled() {
    return this.uri.scheme === 'untitled';
  }
  get isDirty() {
    return false;
  }
  get eol() {
    return 1; // LF
  }
  get lineCount() {
    return this._lineStarts.length;
  }
  getText(range) {
    if (!range) return this._text;
    const s = this.offsetAt(range.start);
    const e = this.offsetAt(range.end);
    return this._text.slice(s, e);
  }
  offsetAt(position) {
    const line = Math.max(0, Math.min(position.line, this._lineStarts.length - 1));
    const lineStart = this._lineStarts[line];
    const lineEnd =
      line + 1 < this._lineStarts.length ? this._lineStarts[line + 1] : this._text.length;
    return Math.min(lineStart + position.character, lineEnd);
  }
  positionAt(offset) {
    offset = Math.max(0, Math.min(offset, this._text.length));
    // Binary search the line.
    let lo = 0;
    let hi = this._lineStarts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this._lineStarts[mid] <= offset) lo = mid;
      else hi = mid - 1;
    }
    return new Position(lo, offset - this._lineStarts[lo]);
  }
  lineAt(lineOrPos) {
    const line = typeof lineOrPos === 'number' ? lineOrPos : lineOrPos.line;
    const start = this._lineStarts[line];
    const end = line + 1 < this._lineStarts.length ? this._lineStarts[line + 1] : this._text.length;
    let text = this._text.slice(start, end);
    text = text.replace(/\r?\n$/, '');
    const firstNon = text.search(/\S/);
    return {
      lineNumber: line,
      text: text,
      range: new Range(new Position(line, 0), new Position(line, text.length)),
      rangeIncludingLineBreak: new Range(new Position(line, 0), new Position(line, end - start)),
      firstNonWhitespaceCharacterIndex: firstNon < 0 ? text.length : firstNon,
      isEmptyOrWhitespace: text.trim().length === 0,
    };
  }
  getWordRangeAtPosition(position, regexp) {
    const re = regexp || /[A-Za-z0-9_]+/g;
    const lineText = this.lineAt(position.line).text;
    re.lastIndex = 0;
    let m;
    while ((m = re.exec(lineText)) !== null) {
      const start = m.index;
      const end = m.index + m[0].length;
      if (position.character >= start && position.character <= end) {
        return new Range(new Position(position.line, start), new Position(position.line, end));
      }
      if (m.index > position.character) break;
    }
    return undefined;
  }
  save() {
    return Promise.resolve(true);
  }
}

class TextEditor {
  constructor(doc) {
    this.document = doc;
    const zero = new Position(0, 0);
    this.selection = new Selection(zero, zero);
    this.selections = [this.selection];
    this.options = { tabSize: 4, insertSpaces: true };
    this.viewColumn = 1;
  }
  edit(callback) {
    const edits = [];
    const builder = {
      replace: (range, text) => edits.push(rangeEditOf(range, text)),
      insert: (position, text) => edits.push(rangeEditOf(new Range(position, position), text)),
      delete: (range) => edits.push(rangeEditOf(range, '')),
      setEndOfLine: () => {},
    };
    callback(builder);
    return requestRust('editor.applyEdits', {
      uri: this.document._uriString,
      edits: edits,
    }).then((res) => {
      if (res && res.applied) {
        this.document._reset(res.version, res.text);
        return true;
      }
      return false;
    });
  }
  revealRange() {}
  setDecorations() {}
}

function rangeEditOf(range, text) {
  return {
    startLine: range.start.line,
    startCharacter: range.start.character,
    endLine: range.end.line,
    endCharacter: range.end.character,
    text: text || '',
  };
}

class Uri {
  constructor(scheme, authority, p) {
    this.scheme = scheme || 'file';
    this.authority = authority || '';
    this.path = p || '';
  }
  get fsPath() {
    return this.path;
  }
  toString() {
    return this.scheme + '://' + this.authority + this.path;
  }
  with(change) {
    return new Uri(
      change.scheme || this.scheme,
      change.authority || this.authority,
      change.path || this.path
    );
  }
  static file(p) {
    return new Uri('file', '', p);
  }
  static parse(s) {
    const m = /^([a-zA-Z][\w+.-]*):\/\/([^/]*)(.*)$/.exec(s);
    return m ? new Uri(m[1], m[2], m[3]) : new Uri('file', '', s);
  }
  static joinPath(base, ...parts) {
    return new Uri(base.scheme, base.authority, path.posix.join(base.path, ...parts));
  }
}

function enumObj(obj) {
  return Object.freeze(obj);
}

function messageSender(level) {
  return function (message, ...rest) {
    // Optional options object may be first; items follow.
    let items = rest;
    if (items.length && typeof items[0] === 'object' && !('title' in items[0])) {
      items = items.slice(1);
    }
    const titles = items.map((i) => (typeof i === 'string' ? i : i && i.title));
    return requestRust('window.showMessage', {
      level: level,
      message: String(message),
      items: titles,
    }).then((choice) => {
      if (choice == null) return undefined;
      const found = items.find((i) => (typeof i === 'string' ? i : i && i.title) === choice);
      return found !== undefined ? found : choice;
    });
  };
}

function memento() {
  const store = {};
  return {
    get(key, def) {
      return store[key] !== undefined ? store[key] : def;
    },
    update(key, value) {
      store[key] = value;
      return Promise.resolve();
    },
    keys() {
      return Object.keys(store);
    },
    setKeysForSync() {},
  };
}

function outputChannel(name) {
  return {
    name: name,
    append: (s) => notifyRust('log', { message: '[' + name + '] ' + s }),
    appendLine: (s) => notifyRust('log', { message: '[' + name + '] ' + s }),
    replace: () => {},
    clear: () => {},
    show: () => {},
    hide: () => {},
    dispose: () => {},
  };
}

const vscode = {
  version: '1.0.0-editor-rs',
  Uri,
  Position,
  Range,
  Selection,
  Location,
  EventEmitter,
  Disposable: Object.assign(
    function Disposable(fn) {
      return makeDisposable(fn);
    },
    {
      from: (...items) => makeDisposable(() => items.forEach((i) => i && i.dispose && i.dispose())),
    }
  ),
  CancellationTokenSource: class {
    constructor() {
      this.token = { isCancellationRequested: false, onCancellationRequested: new EventEmitter().event };
    }
    cancel() {
      this.token.isCancellationRequested = true;
    }
    dispose() {}
  },
  CompletionItem: class {
    constructor(label, kind) {
      this.label = label;
      this.kind = kind;
    }
  },
  CompletionList: class {
    constructor(items, isIncomplete) {
      this.items = items || [];
      this.isIncomplete = !!isIncomplete;
    }
  },
  SnippetString: class {
    constructor(value) {
      this.value = value || '';
    }
    appendText(s) {
      this.value += s;
      return this;
    }
    appendTabstop(n) {
      this.value += '$' + (n || 0);
      return this;
    }
    appendPlaceholder(s, n) {
      this.value += '${' + (n || 1) + ':' + s + '}';
      return this;
    }
  },
  MarkdownString: class {
    constructor(value) {
      this.value = value || '';
    }
    appendText(s) {
      this.value += s;
      return this;
    }
    appendMarkdown(s) {
      this.value += s;
      return this;
    }
    appendCodeblock(code, lang) {
      this.value += '\n```' + (lang || '') + '\n' + code + '\n```\n';
      return this;
    }
  },
  Hover: class {
    constructor(contents, range) {
      this.contents = Array.isArray(contents) ? contents : [contents];
      this.range = range;
    }
  },
  Diagnostic: class {
    constructor(range, message, severity) {
      this.range = range;
      this.message = message;
      this.severity = severity === undefined ? 0 : severity;
    }
  },
  TextEdit: class {
    constructor(range, newText) {
      this.range = range;
      this.newText = newText;
    }
    static replace(range, newText) {
      return { range, newText };
    }
    static insert(position, newText) {
      return { range: new Range(position, position), newText };
    }
  },
  ThemeColor: class {
    constructor(id) {
      this.id = id;
    }
  },
  ThemeIcon: class {
    constructor(id) {
      this.id = id;
    }
  },

  CompletionItemKind: enumObj({
    Text: 0, Method: 1, Function: 2, Constructor: 3, Field: 4, Variable: 5,
    Class: 6, Interface: 7, Module: 8, Property: 9, Keyword: 13, Snippet: 14,
  }),
  DiagnosticSeverity: enumObj({ Error: 0, Warning: 1, Information: 2, Hint: 3 }),
  StatusBarAlignment: enumObj({ Left: 1, Right: 2 }),
  ViewColumn: enumObj({ Active: -1, Beside: -2, One: 1, Two: 2, Three: 3 }),
  ConfigurationTarget: enumObj({ Global: 1, Workspace: 2, WorkspaceFolder: 3 }),
  ExtensionMode: enumObj({ Production: 1, Development: 2, Test: 3 }),
  TextEditorRevealType: enumObj({ Default: 0, InCenter: 1 }),
  EndOfLine: enumObj({ LF: 1, CRLF: 2 }),

  env: {
    appName: 'editor-rs',
    appHost: 'desktop',
    language: 'en',
    machineId: 'editor-rs-machine',
    sessionId: 'editor-rs-session',
    uriScheme: 'editor-rs',
    clipboard: {
      readText: () => Promise.resolve(''),
      writeText: () => Promise.resolve(),
    },
    openExternal: () => Promise.resolve(true),
  },

  commands: {
    registerCommand(id, cb, thisArg) {
      commandMap.set(id, { cb, thisArg });
      notifyRust('command.registered', { id });
      return makeDisposable(() => commandMap.delete(id));
    },
    registerTextEditorCommand(id, cb, thisArg) {
      return this.registerCommand(id, cb, thisArg);
    },
    executeCommand(id, ...args) {
      const entry = commandMap.get(id);
      if (entry) return Promise.resolve(entry.cb.apply(entry.thisArg, args));
      return requestRust('commands.executeCommand', { command: id, args });
    },
    getCommands() {
      return Promise.resolve(Array.from(commandMap.keys()));
    },
  },

  window: {
    showInformationMessage: messageSender('info'),
    showWarningMessage: messageSender('warning'),
    showErrorMessage: messageSender('error'),
    showQuickPick(items, _options) {
      return Promise.resolve(items).then((resolved) => {
        const labels = (resolved || []).map((i) => (typeof i === 'string' ? i : i && i.label));
        return requestRust('window.showQuickPick', { items: labels }).then((choice) => {
          if (choice == null) return undefined;
          const found = (resolved || []).find(
            (i) => (typeof i === 'string' ? i : i && i.label) === choice
          );
          return found !== undefined ? found : choice;
        });
      });
    },
    showInputBox(options) {
      options = options || {};
      return requestRust('window.showInputBox', {
        prompt: options.prompt || null,
        value: options.value || null,
      }).then((v) => (v == null ? undefined : v));
    },
    createOutputChannel: (name) => outputChannel(name),
    setStatusBarMessage: () => makeDisposable(),
    createStatusBarItem: () => ({
      text: '',
      tooltip: '',
      command: undefined,
      show() {},
      hide() {},
      dispose() {},
    }),
    createTextEditorDecorationType: () => ({ dispose() {} }),
    withProgress: (_opts, task) =>
      Promise.resolve(task({ report() {} }, { isCancellationRequested: false })),
    visibleTextEditors: [],
    onDidChangeActiveTextEditor: new EventEmitter().event,
    onDidChangeTextEditorSelection: new EventEmitter().event,
    showTextDocument: () => Promise.resolve(undefined),
  },

  workspace: {
    workspaceFolders: [],
    name: undefined,
    rootPath: undefined,
    getConfiguration(section) {
      const prefix = section ? section + '.' : '';
      return {
        get(key, def) {
          void prefix;
          return def;
        },
        has() {
          return false;
        },
        inspect() {
          return undefined;
        },
        update() {
          return Promise.resolve();
        },
      };
    },
    getWorkspaceFolder: () => undefined,
    asRelativePath: (p) => (p && p.fsPath ? p.fsPath : String(p)),
    onDidChangeConfiguration: new EventEmitter().event,
    onDidOpenTextDocument: new EventEmitter().event,
    onDidCloseTextDocument: new EventEmitter().event,
    onDidChangeTextDocument: new EventEmitter().event,
    onDidSaveTextDocument: new EventEmitter().event,
    onDidChangeWorkspaceFolders: new EventEmitter().event,
    createFileSystemWatcher: () => ({
      onDidCreate: new EventEmitter().event,
      onDidChange: new EventEmitter().event,
      onDidDelete: new EventEmitter().event,
      dispose() {},
    }),
    registerTextDocumentContentProvider: () => makeDisposable(),
    openTextDocument: () => Promise.resolve(undefined),
    applyEdit: () => Promise.resolve(true),
  },

  languages: {
    registerCompletionItemProvider(selector, provider) {
      providers.completion.push({ selector, provider });
      return makeDisposable();
    },
    registerHoverProvider(selector, provider) {
      providers.hover.push({ selector, provider });
      return makeDisposable();
    },
    registerDefinitionProvider(selector, provider) {
      providers.definition.push({ selector, provider });
      return makeDisposable();
    },
    registerCodeActionsProvider(selector, provider) {
      providers.codeAction.push({ selector, provider });
      return makeDisposable();
    },
    registerDocumentFormattingEditProvider: () => makeDisposable(),
    createDiagnosticCollection: (name) => ({
      name,
      set() {},
      delete() {},
      clear() {},
      dispose() {},
      forEach() {},
    }),
    setLanguageConfiguration: () => makeDisposable(),
  },

  extensions: {
    getExtension: () => undefined,
    all: [],
    onDidChange: new EventEmitter().event,
  },
};

// Live views over the document mirror / active editor.
const editors = new Map(); // uri -> TextEditor
function editorFor(uri) {
  const doc = documents.get(uri);
  if (!doc) return undefined;
  let ed = editors.get(uri);
  if (!ed) {
    ed = new TextEditor(doc);
    editors.set(uri, ed);
  } else {
    ed.document = doc;
  }
  return ed;
}
Object.defineProperty(vscode.window, 'activeTextEditor', {
  get() {
    return activeEditorUri ? editorFor(activeEditorUri) : undefined;
  },
});
Object.defineProperty(vscode.workspace, 'textDocuments', {
  get() {
    return Array.from(documents.values());
  },
});
vscode.workspace.openTextDocument = function (uriOrOptions) {
  let key;
  if (typeof uriOrOptions === 'string') key = Uri.file(uriOrOptions).toString();
  else if (uriOrOptions && uriOrOptions.toString) key = uriOrOptions.toString();
  const existing = key && documents.get(key);
  if (existing) return Promise.resolve(existing);
  // Unknown document: hand back an empty in-memory mirror.
  return Promise.resolve(new TextDocument(key || 'untitled:Untitled', 'plaintext', 1, ''));
};

// ---------------------------------------------------------------- require hook
const originalLoad = Module._load;
Module._load = function (request, parent, isMain) {
  if (request === 'vscode') return vscode;
  return originalLoad.call(this, request, parent, isMain);
};

// ---------------------------------------------------------------- activation
function makeContext(extensionPath) {
  return {
    subscriptions: [],
    extensionPath,
    extensionUri: Uri.file(extensionPath),
    globalState: memento(),
    workspaceState: memento(),
    secrets: {
      get: () => Promise.resolve(undefined),
      store: () => Promise.resolve(),
      delete: () => Promise.resolve(),
      onDidChange: new EventEmitter().event,
    },
    environmentVariableCollection: {
      replace() {},
      append() {},
      prepend() {},
      clear() {},
    },
    asAbsolutePath: (p) => path.join(extensionPath, p),
    storageUri: undefined,
    globalStorageUri: Uri.file(path.join(require('os').tmpdir(), 'editor-rs-storage')),
    logUri: Uri.file(path.join(require('os').tmpdir(), 'editor-rs-logs')),
    extensionMode: 1,
    extension: { id: 'editor-rs.ext', packageJSON: {} },
  };
}

async function activateExtension(params) {
  const { extensionPath, main, id } = params;
  if (!main) {
    // Declarative-only extension: nothing to run.
    return { commands: Array.from(commandMap.keys()), activated: false };
  }
  const mainPath = path.resolve(extensionPath, main);
  const mod = require(mainPath);
  const ctx = makeContext(extensionPath);
  activeExtensions.set(id, { mod, ctx });
  if (typeof mod.activate === 'function') {
    await mod.activate(ctx);
  }
  return { commands: Array.from(commandMap.keys()), activated: true };
}

async function runCommand(command, args) {
  const entry = commandMap.get(command);
  if (!entry) {
    throw new Error('command not registered: ' + command);
  }
  return await entry.cb.apply(entry.thisArg, args || []);
}

function deactivateAll() {
  for (const [, { mod, ctx }] of activeExtensions) {
    try {
      if (ctx && Array.isArray(ctx.subscriptions)) {
        for (const d of ctx.subscriptions) {
          if (d && typeof d.dispose === 'function') d.dispose();
        }
      }
      if (mod && typeof mod.deactivate === 'function') mod.deactivate();
    } catch (_) {
      /* ignore */
    }
  }
  activeExtensions.clear();
}

// Signal readiness (useful for debugging; Rust ignores unknown notifications).
notifyRust('host.ready', { pid: process.pid, node: process.version });
