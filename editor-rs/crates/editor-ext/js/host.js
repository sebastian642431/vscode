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

function handleNotification(method, _params) {
  switch (method) {
    case 'shutdown':
      process.exit(0);
      break;
    case 'deactivate':
      deactivateAll();
      break;
  }
}

async function handleRequest(method, params) {
  switch (method) {
    case 'activate':
      return activateExtension(params);
    case 'executeCommand':
      return runCommand(params.command, params.args || []);
    case 'deactivate':
      deactivateAll();
      return null;
    default:
      return null;
  }
}

// ---------------------------------------------------------------- vscode shim
const commandMap = new Map(); // id -> { cb, thisArg }
const providers = { completion: [], hover: [], definition: [], codeAction: [] };
const activeExtensions = new Map(); // id -> { mod, ctx }

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
    activeTextEditor: undefined,
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
