const SESSION_KEY = 'ultramoji4d.emojiWeb.session.v1';
const PKCE_KEY = 'ultramoji4d.emojiWeb.pkce.v1';
const DEFAULT_REDIRECT = `${window.location.origin}${window.location.pathname}`;
const STANDARD_EMOJI_DATA_URL = 'https://cdn.jsdelivr.net/gh/iamcal/emoji-data@master/emoji.json';
const STANDARD_EMOJI_IMAGE_BASE_URL = 'https://cdn.jsdelivr.net/gh/jdecked/twemoji@17.0.2/assets/72x72';
const ASSET_CACHE_MAX_BYTES = 16 * 1024 * 1024;
const FETCH_MAX_BYTES = 8 * 1024 * 1024;
const FETCH_TIMEOUT_MS = 20_000;
const ACTIVE_POLL_MS = 100;
const IDLE_POLL_MS = 1_000;
const SKIN_TONE_UNIFIED_PARTS = new Set(['1f3fb', '1f3fc', '1f3fd', '1f3fe', '1f3ff']);

function debugLogsEnabled() {
  try {
    return new URLSearchParams(window.location.search).has('debug')
      || localStorage.getItem('ultramoji4d.debug') === '1';
  } catch {
    return false;
  }
}

const DEBUG_LOGS = debugLogsEnabled();

function log(...args) {
  if (!DEBUG_LOGS) return;
  console.log('[ultramoji-viewer-4d]', ...args);
}

function createByteCache(maxBytes) {
  let totalBytes = 0;
  const entries = new Map();
  return {
    clear() {
      totalBytes = 0;
      entries.clear();
    },
    has(key) {
      return entries.has(key);
    },
    get(key) {
      const entry = entries.get(key);
      if (!entry) return undefined;
      entries.delete(key);
      entries.set(key, entry);
      return entry.bytes;
    },
    set(key, bytes) {
      const size = bytes?.byteLength ?? 0;
      const previous = entries.get(key);
      if (previous) {
        totalBytes -= previous.size;
        entries.delete(key);
      }
      if (size > maxBytes) {
        return false;
      }
      entries.set(key, { bytes, size });
      totalBytes += size;
      while (totalBytes > maxBytes) {
        const oldestKey = entries.keys().next().value;
        if (oldestKey === undefined) break;
        const oldest = entries.get(oldestKey);
        totalBytes -= oldest?.size ?? 0;
        entries.delete(oldestKey);
      }
      return true;
    },
    bytesUsed() {
      return totalBytes;
    },
  };
}

async function fetchBytesWithLimit(url, options = {}) {
  const timeoutController = new AbortController();
  const timeoutId = window.setTimeout(() => timeoutController.abort(), FETCH_TIMEOUT_MS);
  if (options.signal) {
    if (options.signal.aborted) {
      timeoutController.abort();
    } else {
      options.signal.addEventListener('abort', () => timeoutController.abort(), { once: true });
    }
  }
  try {
    const response = await fetch(url, {
      ...options,
      signal: timeoutController.signal,
    });
    if (!response.ok) {
      throw new Error(`Emoji fetch failed: ${response.status} ${response.statusText}`);
    }
    const contentLength = Number(response.headers.get('content-length') || 0);
    if (contentLength > FETCH_MAX_BYTES) {
      throw new Error(`Emoji asset is too large (${contentLength} bytes)`);
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength > FETCH_MAX_BYTES) {
      throw new Error(`Emoji asset is too large (${bytes.byteLength} bytes)`);
    }
    return bytes;
  } finally {
    window.clearTimeout(timeoutId);
  }
}

function normalizeStandardUnifiedParts(unified) {
  return String(unified ?? '')
    .trim()
    .toLowerCase()
    .replace(/\.png$/i, '')
    .split('-')
    .filter(Boolean)
    .map((part) => {
      if (!/^[0-9a-f]+$/i.test(part)) {
        return part;
      }
      const codepoint = Number.parseInt(part, 16);
      return codepoint.toString(16);
    });
}

export function canonicalizeStandardUnified(unified) {
  const parts = normalizeStandardUnifiedParts(unified);
  const hasZwj = parts.includes('200d');
  return parts
    .filter((part) => hasZwj || part !== 'fe0f')
    .join('-');
}

function hasSkinToneModifier(unified) {
  return canonicalizeStandardUnified(unified)
    .split('-')
    .some((part) => SKIN_TONE_UNIFIED_PARTS.has(part));
}

export function standardEmojiImageFilename(entry, unified) {
  const image = String(entry?.image ?? '').trim();
  const parts = normalizeStandardUnifiedParts(image || unified);
  const hasZwj = parts.includes('200d');
  return `${parts.filter((part) => hasZwj || part !== 'fe0f').join('-')}.png`;
}

export function standardEmojiImageFilenames(entry, unified) {
  const image = String(entry?.image ?? '').trim();
  const parts = normalizeStandardUnifiedParts(image || unified);
  const strippedFilename = `${parts.filter((part) => part !== 'fe0f').join('-')}.png`;
  return uniqueNames([standardEmojiImageFilename(entry, unified), strippedFilename]);
}

function readConfig() {
  const config = window.SLACK_EMOJI_APP_CONFIG ?? {};
  return {
    clientId: String(config.clientId ?? '').trim(),
    redirectUri: String(config.redirectUri ?? DEFAULT_REDIRECT).trim() || DEFAULT_REDIRECT,
    userScope: String(config.userScope ?? 'emoji:read').trim() || 'emoji:read',
  };
}

function loadSession() {
  try {
    const raw = localStorage.getItem(SESSION_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function saveSession(session) {
  localStorage.setItem(SESSION_KEY, JSON.stringify(session));
}

function clearSession() {
  localStorage.removeItem(SESSION_KEY);
}

function loadPkce() {
  try {
    const raw = localStorage.getItem(PKCE_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function savePkce(pkce) {
  localStorage.setItem(PKCE_KEY, JSON.stringify(pkce));
}

function clearPkce() {
  localStorage.removeItem(PKCE_KEY);
}

function cleanOAuthParams(url = new URL(window.location.href)) {
  const cleaned = new URL(url.toString());
  cleaned.searchParams.delete('code');
  cleaned.searchParams.delete('state');
  cleaned.searchParams.delete('error');
  cleaned.searchParams.delete('error_description');
  return cleaned;
}

function sameOriginUrl(rawUrl) {
  try {
    const url = new URL(rawUrl, window.location.origin);
    if (url.origin !== window.location.origin) {
      return null;
    }
    return url;
  } catch {
    return null;
  }
}

function currentReturnUrl() {
  return cleanOAuthParams().toString();
}

function parseRoute() {
  const pathParts = window.location.pathname.split('/').filter(Boolean);
  const emojiName = pathParts[0] === 'emoji' && pathParts[1]
    ? decodeURIComponent(pathParts.slice(1).join('/'))
    : '';
  return {
    emojiName,
    search: new URLSearchParams(window.location.search).get('q') || '',
  };
}

function routeSignature(route) {
  return `${route.emojiName}\n${route.search}`;
}

function routeUrl(route) {
  const url = new URL(window.location.href);
  url.pathname = route.emojiName ? `/emoji/${encodeURIComponent(route.emojiName)}` : '/';
  url.search = '';
  if (route.search) {
    url.searchParams.set('q', route.search);
  }
  url.hash = '';
  return url;
}

function currentRouteUrl() {
  const url = new URL(window.location.href);
  url.searchParams.delete('code');
  url.searchParams.delete('state');
  url.searchParams.delete('error');
  url.searchParams.delete('error_description');
  return `${url.pathname}${url.search}${url.hash}`;
}

function routeUrlPath(route) {
  const url = routeUrl(route);
  return `${url.pathname}${url.search}${url.hash}`;
}

function applyRouteToWasm(state) {
  const route = parseRoute();
  state.lastAppliedRouteSignature = routeSignature(route);
  state.wasm.set_route_state(route.search, route.emojiName);
  return route;
}

function syncUrlFromWasm(state) {
  const route = {
    emojiName: state.wasm.current_preview_emoji_name(),
    search: state.wasm.current_search_query(),
  };
  const signature = routeSignature(route);
  if (signature === state.lastWasmRouteSignature) {
    return;
  }
  state.lastWasmRouteSignature = signature;
  const nextPath = routeUrlPath(route);
  if (nextPath !== currentRouteUrl()) {
    history.replaceState({}, '', nextPath);
  }
}

function pushUiState(wasm, {
  status,
  workspace = '',
  hint = '',
  signedIn = false,
  busy = false,
  loginEnabled = false,
  catalogReady = false,
}) {
  wasm.set_hosted_auth_state(status, workspace, hint, signedIn, busy, loginEnabled, catalogReady);
}

function applyUiState(state, fields) {
  state.ui = {
    status: '',
    workspace: '',
    hint: '',
    signedIn: false,
    busy: false,
    loginEnabled: false,
    catalogReady: false,
    ...state.ui,
    ...fields,
  };
  if (!state.ui.signedIn) {
    state.ui.workspace = '';
  }
  log('ui state', {
    status: state.ui.status,
    signedIn: state.ui.signedIn,
    busy: state.ui.busy,
    loginEnabled: state.ui.loginEnabled,
    catalogReady: state.ui.catalogReady,
    workspace: state.ui.workspace,
  });
  pushUiState(state.wasm, state.ui);
  state.updateKeypad?.();
}

const KEYPAD_BUTTONS = [
  { slot: 'esc', key: 'Escape', label: 'Esc' },
  { slot: 'up', key: 'ArrowUp', label: 'Up' },
  { slot: 'pgup', key: 'PageUp', label: 'PgUp' },
  { slot: 'left', key: 'ArrowLeft', label: 'Left' },
  { slot: 'enter', key: 'Enter', label: 'Enter' },
  { slot: 'right', key: 'ArrowRight', label: 'Right' },
  { slot: 'f2', key: 'F2', label: 'F2', small: true },
  { slot: 'd', key: 'd', label: 'D', small: true, hideWhenDisabled: true },
  { slot: 'down', key: 'ArrowDown', label: 'Down' },
  { slot: 'pgdn', key: 'PageDown', label: 'PgDn' },
];

function shouldShowMobileKeypad() {
  const hasTouch = navigator.maxTouchPoints > 0;
  const coarsePointer = window.matchMedia?.('(pointer: coarse)').matches ?? false;
  const noHover = window.matchMedia?.('(hover: none)').matches ?? false;
  const smallViewport = Math.min(window.innerWidth, window.innerHeight) <= 820;
  const mobileUserAgent = /Android|iPhone|iPad|iPod|Mobile/i.test(navigator.userAgent || '');

  return (hasTouch || coarsePointer || mobileUserAgent) && (noHover || smallViewport || mobileUserAgent);
}

function readKeypadMode(state) {
  try {
    return state.wasm.current_keypad_mode?.() || 'auth';
  } catch {
    return 'auth';
  }
}

function keypadActionFor(def, state, mode) {
  const search = state.wasm.current_search_query();
  const previousPreview = state.wasm.previous_preview_emoji_name();
  const nextPreview = state.wasm.next_preview_emoji_name();
  const loginReady = !state.ui.signedIn && !state.ui.busy && state.ui.loginEnabled;
  const defaultReady = !state.ui.signedIn
    && !state.ui.busy
    && !state.modeSelected
    && state.standardCatalog.names.length > 0;
  const isGallery = mode === 'gallery';
  const isPreview = mode === 'preview';
  const isSettings = mode === 'settings';
  const isAuth = mode === 'auth';
  const noAction = { action: 'No action', enabled: false };

  if (def.ctrlKey && def.key === 'Backspace') {
    if (isPreview) return { action: 'Close', enabled: true };
    if (isGallery && search) return { action: 'Delete word', enabled: true };
    return noAction;
  }

  switch (def.key) {
    case 'F2':
      if (isGallery && state.ui.signedIn) return { action: 'Settings', enabled: true };
      if ((isAuth || isGallery) && loginReady) return { action: 'Slack login', enabled: true };
      return noAction;
    case 'd':
      return defaultReady ? { action: 'Default emoji', enabled: true } : noAction;
    case 'Escape':
      if (isSettings) return { action: 'Back', enabled: true };
      if (isPreview) return { action: 'Close', enabled: true };
      if (isGallery && search) return { action: 'Clear search', enabled: true };
      return noAction;
    case 'PageUp':
      return isGallery ? { action: 'Page up', enabled: true } : noAction;
    case 'PageDown':
      return isGallery ? { action: 'Page down', enabled: true } : noAction;
    case 'ArrowUp':
      if (isPreview) {
        return previousPreview
          ? { action: 'Prev emoji', enabled: true }
          : { action: 'No previous', enabled: false };
      }
      return isGallery ? { action: 'Select up', enabled: true } : noAction;
    case 'ArrowDown':
      if (isPreview) {
        return nextPreview
          ? { action: 'Next emoji', enabled: true }
          : { action: 'No next', enabled: false };
      }
      return isGallery ? { action: 'Select down', enabled: true } : noAction;
    case 'ArrowLeft':
      if (isPreview) return { action: 'Close', enabled: true };
      return isGallery ? { action: 'Prev tab', enabled: true } : noAction;
    case 'ArrowRight':
      if (isPreview) return { action: 'Close', enabled: true };
      return isGallery ? { action: 'Next tab', enabled: true } : noAction;
    case 'Enter':
      if (isAuth && loginReady) return { action: 'Slack login', enabled: true };
      if (isSettings) return { action: 'Sign out', enabled: true };
      if (isGallery) return { action: 'View emoji', enabled: true };
      return noAction;
    case 'Backspace':
      if (isPreview) return { action: 'Close', enabled: true };
      if (isGallery && search) return { action: 'Delete char', enabled: true };
      return noAction;
    case ' ':
      return isGallery ? { action: 'Search space', enabled: true } : noAction;
    default:
      return noAction;
  }
}

function dispatchSyntheticKey(key, options = {}) {
  const event = new KeyboardEvent('keydown', {
    key,
    bubbles: true,
    cancelable: true,
    ctrlKey: Boolean(options.ctrlKey),
    altKey: Boolean(options.altKey),
  });
  window.dispatchEvent(event);
}

function installMobileKeypad(state) {
  const keypad = document.createElement('section');
  keypad.id = 'mobile-keypad';
  keypad.setAttribute('aria-label', 'Mobile keyboard controls');

  const pad = document.createElement('div');
  pad.className = 'keypad-pad';
  keypad.append(pad);

  const buttons = [];
  const addButton = (parent, def) => {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `keypad-button${def.small ? ' keypad-small' : ''}`;
    button.innerHTML = '<span class="keypad-key"></span><span class="keypad-action"></span>';
    button.addEventListener('pointerdown', (event) => event.preventDefault());
    button.addEventListener('click', () => {
      if (button.disabled) return;
      dispatchSyntheticKey(def.key, def);
      scheduleUpdate();
    });
    buttons.push({ def, button });
    parent.append(button);
  };

  const bySlot = Object.fromEntries(KEYPAD_BUTTONS.map((def) => [def.slot, def]));
  addButton(pad, bySlot.esc);
  addButton(pad, bySlot.up);
  addButton(pad, bySlot.pgup);
  addButton(pad, bySlot.left);
  addButton(pad, bySlot.enter);
  addButton(pad, bySlot.right);

  const comboCell = document.createElement('div');
  comboCell.className = 'keypad-combo';
  addButton(comboCell, bySlot.f2);
  addButton(comboCell, bySlot.d);
  pad.append(comboCell);

  addButton(pad, bySlot.down);
  addButton(pad, bySlot.pgdn);

  const searchRow = document.createElement('div');
  searchRow.className = 'keypad-search';
  const searchInput = document.createElement('input');
  searchInput.id = 'keypad-search-input';
  searchInput.type = 'text';
  searchInput.autocapitalize = 'none';
  searchInput.autocomplete = 'off';
  searchInput.autocorrect = 'off';
  searchInput.spellcheck = false;
  searchInput.placeholder = 'PRINTABLE KEYS / SEARCH';
  searchInput.setAttribute('aria-label', 'Printable key search input');
  searchInput.addEventListener('beforeinput', (event) => {
    if (readKeypadMode(state) !== 'gallery') {
      event.preventDefault();
      return;
    }
    const inputType = event.inputType || '';
    if (inputType === 'insertText' || inputType === 'insertCompositionText') {
      event.preventDefault();
      for (const ch of String(event.data || '')) {
        dispatchSyntheticKey(ch);
      }
      scheduleUpdate();
    } else if (inputType === 'deleteContentBackward') {
      event.preventDefault();
      dispatchSyntheticKey('Backspace');
      scheduleUpdate();
    } else if (inputType === 'deleteWordBackward') {
      event.preventDefault();
      dispatchSyntheticKey('Backspace', { ctrlKey: true });
      scheduleUpdate();
    } else {
      event.preventDefault();
    }
  });
  searchInput.addEventListener('keydown', (event) => {
    if (event.key !== 'Backspace' && event.key !== 'Enter' && event.key !== 'Escape') {
      return;
    }
    event.preventDefault();
    if (event.key === 'Backspace') {
      dispatchSyntheticKey('Backspace', { ctrlKey: event.ctrlKey || event.altKey });
    } else {
      dispatchSyntheticKey(event.key);
      searchInput.blur();
    }
    scheduleUpdate();
  });
  searchRow.append(searchInput);
  keypad.append(searchRow);
  document.body.append(keypad);

  function update() {
    const mode = readKeypadMode(state);
    for (const { def, button } of buttons) {
      const { action, enabled } = keypadActionFor(def, state, mode);
      button.querySelector('.keypad-key').textContent = def.label;
      button.querySelector('.keypad-action').textContent = action;
      const hidden = Boolean(def.hideWhenDisabled && !enabled);
      button.classList.toggle('keypad-hidden', hidden);
      if (def.slot === 'd') {
        comboCell.classList.toggle('keypad-d-hidden', hidden);
      }
      button.disabled = !enabled || hidden;
      button.setAttribute('aria-label', `${def.label}: ${action}`);
      button.title = `${def.label}: ${action}`;
    }
    const search = state.wasm.current_search_query();
    if (document.activeElement !== searchInput || searchInput.value !== search) {
      searchInput.value = search;
    }
    searchInput.disabled = mode !== 'gallery';
    searchInput.placeholder = mode === 'gallery' ? 'PRINTABLE KEYS / SEARCH' : 'SEARCH DISABLED';
  }

  function scheduleUpdate() {
    window.requestAnimationFrame(update);
    window.setTimeout(update, 80);
  }

  update();
  return { destroy: () => keypad.remove(), update };
}

function installConditionalMobileKeypad(state) {
  let installed = null;
  const reconcile = () => {
    if (shouldShowMobileKeypad()) {
      if (!installed) {
        installed = installMobileKeypad(state);
        state.updateKeypad = installed.update;
      }
      state.updateKeypad?.();
      return;
    }
    if (installed) {
      installed.destroy();
      installed = null;
    }
    state.updateKeypad = null;
  };

  window.addEventListener('resize', reconcile, { passive: true });
  window.addEventListener('orientationchange', reconcile, { passive: true });
  reconcile();
}

function base64Url(bytes) {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

async function sha256(text) {
  const data = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest('SHA-256', data);
  return new Uint8Array(digest);
}

function randomString(byteLength = 32) {
  const bytes = new Uint8Array(byteLength);
  crypto.getRandomValues(bytes);
  return base64Url(bytes);
}

function oauthFormBody(fields) {
  const body = new URLSearchParams();
  for (const [key, value] of Object.entries(fields)) {
    if (value !== undefined && value !== null && value !== '') {
      body.set(key, String(value));
    }
  }
  return body;
}

async function postSlackForm(url, fields) {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded;charset=UTF-8',
    },
    body: oauthFormBody(fields),
  });
  const json = await response.json();
  if (!json.ok) {
    throw new Error(json.error || `Slack API error from ${url}`);
  }
  return json;
}

function extractTokenPayload(payload) {
  const authedUser = payload.authed_user ?? {};
  const accessToken = authedUser.access_token ?? payload.access_token ?? '';
  const refreshToken = authedUser.refresh_token ?? payload.refresh_token ?? '';
  const expiresIn = Number(authedUser.expires_in ?? payload.expires_in ?? 0);
  return {
    accessToken,
    refreshToken,
    expiresAt: expiresIn > 0 ? Date.now() + expiresIn * 1000 : null,
    scope: authedUser.scope ?? payload.scope ?? '',
    team: payload.team ?? null,
  };
}

async function buildPkceLoginUrl(config, returnUrl = currentReturnUrl()) {
  const state = randomString(24);
  const verifier = randomString(48);
  const challenge = base64Url(await sha256(verifier));
  savePkce({ state, verifier, redirectUri: config.redirectUri, returnUrl });
  const url = new URL('https://slack.com/oauth/v2/authorize');
  url.searchParams.set('client_id', config.clientId);
  url.searchParams.set('redirect_uri', config.redirectUri);
  url.searchParams.set('user_scope', config.userScope);
  url.searchParams.set('state', state);
  url.searchParams.set('code_challenge', challenge);
  url.searchParams.set('code_challenge_method', 'S256');
  return url.toString();
}

async function maybeHandleOAuthCallback(config) {
  const url = new URL(window.location.href);
  const code = url.searchParams.get('code');
  const state = url.searchParams.get('state');
  const error = url.searchParams.get('error');
  const pkce = loadPkce();
  const fallbackUrl = cleanOAuthParams(url).toString();
  const returnUrl = sameOriginUrl(pkce?.returnUrl)?.toString() || fallbackUrl;
  if (error) {
    history.replaceState({}, '', returnUrl);
    throw new Error(`Slack authorization failed: ${error}`);
  }
  if (!code) {
    return null;
  }
  log('handling oauth callback');

  if (!pkce || pkce.state !== state) {
    clearPkce();
    history.replaceState({}, '', fallbackUrl);
    log('ignoring stale oauth callback due to state mismatch');
    return null;
  }

  const response = await postSlackForm('https://slack.com/api/oauth.v2.access', {
    client_id: config.clientId,
    code,
    redirect_uri: pkce.redirectUri || config.redirectUri,
    code_verifier: pkce.verifier,
  });
  clearPkce();
  history.replaceState({}, '', returnUrl);

  const session = {
    ...extractTokenPayload(response),
    installedAt: Date.now(),
  };
  if (!session.accessToken) {
    throw new Error('Slack OAuth response did not include a user access token');
  }
  log('oauth callback complete', {
    team: session.team?.name ?? '',
    hasRefresh: Boolean(session.refreshToken),
    hasExpiry: Boolean(session.expiresAt),
  });
  saveSession(session);
  return session;
}

async function ensureFreshSession(config, session) {
  if (!session?.refreshToken || !session?.expiresAt) {
    log('session does not require refresh');
    return session;
  }
  if (session.expiresAt - Date.now() > 60_000) {
    log('session still fresh');
    return session;
  }
  log('refreshing slack session');

  const response = await postSlackForm('https://slack.com/api/oauth.v2.access', {
    client_id: config.clientId,
    grant_type: 'refresh_token',
    refresh_token: session.refreshToken,
  });
  const refreshed = {
    ...session,
    ...extractTokenPayload(response),
    refreshedAt: Date.now(),
  };
  if (!refreshed.accessToken) {
    throw new Error('Slack token refresh did not return an access token');
  }
  log('session refresh complete', {
    team: refreshed.team?.name ?? session.team?.name ?? '',
  });
  saveSession(refreshed);
  return refreshed;
}

function normalizeCategoryName(name, fallback = 'emoji') {
  const normalized = String(name ?? '').trim().toLowerCase().replace(/\s+/g, ' ');
  return normalized || fallback;
}

function uniqueNames(names) {
  const seen = new Set();
  const result = [];
  for (const rawName of Array.isArray(names) ? names : []) {
    const name = String(rawName ?? '').trim();
    if (name && !seen.has(name)) {
      seen.add(name);
      result.push(name);
    }
  }
  return result;
}

function extractCategoryNames(category) {
  for (const key of ['emoji_names', 'emojiNames', 'emojis', 'names', 'emoji']) {
    const value = category?.[key];
    if (Array.isArray(value)) {
      return uniqueNames(value);
    }
  }
  return [];
}

function extractSlackCategories(rawCategories) {
  const categories = [];
  for (const category of Array.isArray(rawCategories) ? rawCategories : []) {
    const name = normalizeCategoryName(category?.name || category?.label || category?.title, '');
    const names = extractCategoryNames(category);
    if (name && names.length > 0) {
      categories.push({ name, names });
    }
  }
  return categories;
}

function resolveEmojiCatalog(rawEmoji, rawCategories = []) {
  const resolvedUrl = new Map();
  const resolutionState = new Set();

  function resolveValue(name) {
    if (resolvedUrl.has(name)) {
      return resolvedUrl.get(name);
    }
    if (resolutionState.has(name)) {
      return null;
    }
    resolutionState.add(name);
    const value = rawEmoji?.[name];
    let resolved = null;
    if (typeof value === 'string') {
      if (value.startsWith('alias:')) {
        resolved = resolveValue(value.slice(6).trim());
      } else if (/^https?:\/\//i.test(value)) {
        resolved = value;
      }
    } else if (value && typeof value.url === 'string' && /^https?:\/\//i.test(value.url)) {
      resolved = value.url;
    }
    resolutionState.delete(name);
    if (resolved) {
      resolvedUrl.set(name, resolved);
    }
    return resolved;
  }

  const names = Object.keys(rawEmoji || {})
    .filter((name) => Boolean(resolveValue(name)))
    .sort((a, b) => a.localeCompare(b));

  const slackCategories = extractSlackCategories(rawCategories);
  const categorizedNames = new Set(slackCategories.flatMap((category) => category.names));
  const customNames = names.filter((name) => !categorizedNames.has(name));
  const categories = slackCategories.length > 0
    ? [
        ...slackCategories,
        ...(customNames.length > 0 ? [{ name: 'custom', names: customNames }] : []),
      ]
    : [{ name: 'custom', names }];

  return {
    names,
    assetUrls: resolvedUrl,
    categories,
    hasSlackCategories: slackCategories.length > 0,
  };
}

async function fetchEmojiCatalog(session) {
  log('fetching emoji catalog', { team: session.team?.name ?? '' });
  const payload = await postSlackForm('https://slack.com/api/emoji.list', {
    token: session.accessToken,
    include_categories: 'true',
  });
  return resolveEmojiCatalog(payload.emoji, payload.categories);
}

async function fetchStandardEmojiCatalog() {
  log('fetching standard emoji catalog');
  const response = await fetch(STANDARD_EMOJI_DATA_URL, {
    method: 'GET',
    mode: 'cors',
    credentials: 'omit',
  });
  if (!response.ok) {
    throw new Error(`Standard emoji catalog fetch failed: ${response.status} ${response.statusText}`);
  }
  const entries = await response.json();
  const assetUrls = new Map();
  for (const entry of Array.isArray(entries) ? entries : []) {
    const unified = canonicalizeStandardUnified(entry?.unified);
    if (!unified || hasSkinToneModifier(unified)) {
      continue;
    }
    const shortNames = Array.isArray(entry?.short_names) ? entry.short_names : [];
    if (shortNames.length === 0) {
      continue;
    }
    const urls = standardEmojiImageFilenames(entry, unified)
      .map((filename) => `${STANDARD_EMOJI_IMAGE_BASE_URL}/${filename}`);
    for (const shortName of shortNames) {
      const name = String(shortName ?? '').trim();
      if (name) {
        assetUrls.set(name, urls.length === 1 ? urls[0] : urls);
      }
    }
  }
  const names = Array.from(assetUrls.keys()).sort((a, b) => a.localeCompare(b));
  return { names, assetUrls, categories: [{ name: 'default', names }] };
}

function mergeCatalogs(...catalogs) {
  const assetUrls = new Map();
  const hasSlackCategories = catalogs.some((catalog) => catalog?.hasSlackCategories);
  for (const catalog of catalogs) {
    if (!catalog?.assetUrls) {
      continue;
    }
    for (const [name, url] of catalog.assetUrls.entries()) {
      assetUrls.set(name, url);
    }
  }

  const categorized = new Set();
  const categories = [];
  for (const catalog of catalogs) {
    if (!catalog?.categories || (hasSlackCategories && !catalog.hasSlackCategories)) {
      continue;
    }
    for (const category of catalog.categories) {
      const names = [];
      for (const name of uniqueNames(category.names)) {
        if (assetUrls.has(name) && !categorized.has(name)) {
          categorized.add(name);
          names.push(name);
        }
      }
      if (names.length > 0) {
        categories.push({ name: normalizeCategoryName(category.name), names });
      }
    }
  }

  const uncategorized = Array.from(assetUrls.keys())
    .filter((name) => !categorized.has(name))
    .sort((a, b) => a.localeCompare(b));
  if (uncategorized.length > 0) {
    categories.push({ name: hasSlackCategories ? 'default' : 'emoji', names: uncategorized });
  }

  const names = categories.flatMap((category) => category.names);
  return { names, assetUrls, categories };
}

function serializeGalleryEntries(categories) {
  const lines = [];
  for (const category of categories) {
    for (const name of category.names) {
      lines.push(`${category.name}\t${name}`);
    }
  }
  return lines.join('\n');
}

function applyMergedCatalog(state) {
  const merged = mergeCatalogs(state.standardCatalog, state.workspaceCatalog);
  state.assetUrls = merged.assetUrls;
  state.assetCache.clear();
  state.failedEmojiNames.clear();
  state.decodedEmojiNames.clear();
  state.preloadingEmojiNames.clear();
  state.currentEmojiName = '';
  state.loadedEmojiName = '';
  state.currentRequestId += 1;
  state.wasm.clear_decoded_emoji_texture_cache();
  state.wasm.set_gallery_entries(serializeGalleryEntries(merged.categories));
  state.wasm.clear_active_emoji_texture();
  return merged;
}

function modeChoiceHint(config, standardCount) {
  if (standardCount === 0) {
    return config.clientId
      ? 'Press ENTER or F2 to open Slack login.'
      : 'Default emoji catalog is unavailable.';
  }
  return config.clientId
    ? 'Press ENTER or F2 for Slack login. Press D for default emojis.'
    : 'Press D for default emojis.';
}

function showModeChoice(state, status = 'SELECT EMOJI MODE') {
  state.modeSelected = false;
  state.assetUrls = new Map();
  state.assetCache.clear();
  state.failedEmojiNames.clear();
  state.decodedEmojiNames.clear();
  state.preloadingEmojiNames.clear();
  state.currentEmojiName = '';
  state.loadedEmojiName = '';
  state.currentRequestId += 1;
  state.wasm.clear_decoded_emoji_texture_cache();
  state.wasm.set_gallery_entries('');
  state.wasm.clear_active_emoji_texture();
  applyUiState(state, {
    status,
    workspace: '',
    hint: modeChoiceHint(state.config, state.standardCatalog.names.length),
    signedIn: false,
    busy: false,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: false,
  });
}

function enableDefaultEmojiMode(state) {
  state.modeSelected = true;
  state.workspaceCatalog = { names: [], assetUrls: new Map() };
  const merged = applyMergedCatalog(state);
  applyRouteToWasm(state);
  applyUiState(state, {
    status: merged.names.length > 0 ? `LOADED ${merged.names.length} DEFAULT EMOJI` : 'DEFAULT EMOJI UNAVAILABLE',
    hint: state.config.clientId ? 'Press F2 to add workspace emoji with Slack.' : '',
    signedIn: false,
    busy: false,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: merged.names.length > 0,
  });
}

function isSlackAssetUrl(rawUrl) {
  try {
    const parsed = new URL(rawUrl);
    const hostname = (parsed.hostname || '').toLowerCase();
    return hostname === 'slack-edge.com'
      || hostname.endsWith('.slack-edge.com')
      || hostname === 'slack-files.com'
      || hostname.endsWith('.slack-files.com');
  } catch {
    return false;
  }
}

async function fetchEmojiBytes(url, signal) {
  if (!isSlackAssetUrl(url)) {
    log('fetching public emoji bytes', { sourceUrl: url });
    const bytes = await fetchBytesWithLimit(url, {
      method: 'GET',
      mode: 'cors',
      credentials: 'omit',
      signal,
    });
    log('public emoji bytes fetched', { sourceUrl: url, byteLength: bytes.byteLength });
    return bytes;
  }

  const relayUrl = new URL('/emoji-asset', window.location.origin);
  relayUrl.searchParams.set('url', url);
  log('fetching emoji bytes', { relayUrl: relayUrl.toString(), sourceUrl: url });
  const bytes = await fetchBytesWithLimit(relayUrl, {
    method: 'GET',
    credentials: 'same-origin',
    signal,
  });
  log('emoji bytes fetched', { sourceUrl: url, byteLength: bytes.byteLength });
  return bytes;
}

function emojiAssetUrls(value) {
  if (Array.isArray(value)) {
    return uniqueNames(value);
  }
  if (typeof value === 'string' && value) {
    return [value];
  }
  return [];
}

function cachedEmojiBytes(assetCache, urls) {
  for (const url of urls) {
    if (assetCache.has(url)) {
      return { url, bytes: assetCache.get(url) };
    }
  }
  return null;
}

async function fetchFirstEmojiBytes(urls, signal) {
  let lastError = null;
  for (const url of urls) {
    try {
      const bytes = await fetchEmojiBytes(url, signal);
      return { url, bytes };
    } catch (error) {
      lastError = error;
      log('emoji bytes candidate failed', { sourceUrl: url, error: String(error?.message || error) });
    }
  }
  throw lastError || new Error('No emoji asset URL candidates');
}

function waitForIdle(timeout = 500) {
  return new Promise((resolve) => {
    if ('requestIdleCallback' in window) {
      window.requestIdleCallback(() => resolve(), { timeout });
    } else {
      window.setTimeout(resolve, 0);
    }
  });
}

function installStorageSync(state) {
  window.addEventListener('storage', async (event) => {
    if (event.key !== SESSION_KEY) {
      return;
    }
    state.session = loadSession();
    if (state.session) {
      try {
        state.session = await ensureFreshSession(state.config, state.session);
        await syncCatalog(state);
      } catch (error) {
        clearSession();
        state.session = null;
        state.workspaceCatalog = { names: [], assetUrls: new Map() };
        showModeChoice(state, 'SLACK SESSION FAILED');
      }
    } else {
      state.workspaceCatalog = { names: [], assetUrls: new Map() };
      showModeChoice(state);
    }
  });
}

export async function bootHostedEmojiApp(wasm) {
  const config = readConfig();
  const state = {
    wasm,
    config,
    session: loadSession(),
    assetUrls: new Map(),
    standardCatalog: { names: [], assetUrls: new Map() },
    workspaceCatalog: { names: [], assetUrls: new Map() },
    assetCache: createByteCache(ASSET_CACHE_MAX_BYTES),
    failedEmojiNames: new Set(),
    decodedEmojiNames: new Set(),
    preloadingEmojiNames: new Set(),
    activeAssetAbortController: null,
    currentEmojiName: '',
    loadedEmojiName: '',
    currentRequestId: 0,
    modeSelected: Boolean(loadSession()),
    signOutRequestSeen: 0,
    lastAppliedRouteSignature: '',
    lastWasmRouteSignature: '',
    autoLoginStarted: false,
    ui: {
      status: '',
      workspace: '',
      hint: '',
      signedIn: Boolean(loadSession()),
      busy: false,
      loginEnabled: Boolean(config.clientId),
      catalogReady: false,
    },
    updateKeypad: null,
  };
  installConditionalMobileKeypad(state);
  log('boot hosted app', {
    hasClientId: Boolean(config.clientId),
    hasSession: Boolean(state.session),
  });

  const startLogin = async ({ popup: usePopup = true } = {}) => {
    let popupWindow = null;
    try {
      if (!config.clientId) {
        applyUiState(state, {
          status: 'LOGIN NOT CONFIGURED',
          hint: 'Set window.SLACK_EMOJI_APP_CONFIG.clientId before using hosted auth.',
          signedIn: false,
          busy: false,
          loginEnabled: false,
          catalogReady: state.ui.catalogReady,
        });
        return;
      }
      const returnUrl = currentReturnUrl();
      if (usePopup) {
        popupWindow = window.open('', '_blank');
      }
      if (usePopup && !popupWindow) {
        applyUiState(state, {
          status: 'POPUP BLOCKED',
          hint: 'Allow popups for this site, then press ENTER again.',
          signedIn: Boolean(state.session),
          workspace: state.session?.team?.name || '',
          busy: false,
          loginEnabled: true,
          catalogReady: state.ui.catalogReady,
        });
        return;
      }
      try {
        popupWindow?.document.write(`<!doctype html><html><head><title>Slack Login</title><style>html,body{height:100%;margin:0}body{display:flex;align-items:center;justify-content:center;background:#0c121c;color:#d6e8ff;font:16px monospace}</style></head><body>Connecting to Slack...</body></html>`);
        popupWindow?.document.close();
      } catch {}
      const loginUrl = await buildPkceLoginUrl(config, returnUrl);
      log(usePopup ? 'opening slack login tab' : 'redirecting to slack login', { loginUrl, returnUrl });
      state.modeSelected = true;
      if (popupWindow) {
        popupWindow.location.href = loginUrl;
      } else {
        window.location.assign(loginUrl);
        return;
      }
      applyUiState(state, {
        status: 'OPENED SLACK LOGIN',
        hint: 'Complete sign-in in the new tab. This window will update automatically.',
        signedIn: Boolean(state.session),
        workspace: state.session?.team?.name || '',
        busy: false,
        loginEnabled: true,
        catalogReady: state.ui.catalogReady,
      });
    } catch (error) {
      try {
        popupWindow?.close();
      } catch {}
      applyUiState(state, {
        status: 'UNABLE TO START SLACK SIGN-IN',
        hint: String(error.message || error),
        signedIn: Boolean(state.session),
        workspace: state.session?.team?.name || '',
        busy: false,
        loginEnabled: Boolean(config.clientId),
        catalogReady: state.ui.catalogReady,
      });
    }
  };

  const openLoginTab = () => startLogin({ popup: true });

  const signOut = () => {
    log('signing out');
    clearSession();
    clearPkce();
    state.session = null;
    state.workspaceCatalog = { names: [], assetUrls: new Map() };
    showModeChoice(state);
  };

  window.addEventListener('keydown', (event) => {
    const loginKey = event.key === 'F2'
      || (event.key === 'Enter' && !state.ui.catalogReady);
    const defaultModeKey = String(event.key || '').toLowerCase() === 'd';
    if (
      defaultModeKey
      && !state.ui.signedIn
      && !state.ui.busy
      && !state.modeSelected
      && state.standardCatalog.names.length > 0
    ) {
      event.preventDefault();
      enableDefaultEmojiMode(state);
      return;
    }
    if (!loginKey) {
      return;
    }
    log('keydown', {
      key: event.key,
      signedIn: state.ui.signedIn,
      busy: state.ui.busy,
      loginEnabled: state.ui.loginEnabled,
    });
    if (
      state.ui.signedIn
      || state.ui.busy
      || !state.ui.loginEnabled
    ) {
      return;
    }
    event.preventDefault();
    void openLoginTab();
  });

  installStorageSync(state);
  window.addEventListener('popstate', () => {
    applyRouteToWasm(state);
  });

  try {
    applyUiState(state, {
      status: 'INITIALIZING',
      hint: 'Loading standard emoji catalog.',
      signedIn: Boolean(state.session),
      busy: true,
      loginEnabled: Boolean(config.clientId),
      catalogReady: false,
    });

    const standardCatalog = await fetchStandardEmojiCatalog();
    log('standard emoji catalog loaded', { count: standardCatalog.names.length });
    state.standardCatalog = standardCatalog;
    const initialRoute = applyRouteToWasm(state);

    if (config.clientId) {
      const callbackSession = await maybeHandleOAuthCallback(config);
      if (callbackSession) {
        state.session = callbackSession;
        state.modeSelected = true;
        applyRouteToWasm(state);
        if (window.opener && window.opener !== window) {
          applyUiState(state, {
            status: 'LOGIN COMPLETE',
            workspace: callbackSession.team?.name || '',
            hint: 'Return to the original tab. This tab will close if the browser allows it.',
            signedIn: true,
            busy: false,
            loginEnabled: true,
            catalogReady: false,
          });
          setTimeout(() => window.close(), 150);
          return;
        }
      }
    }

    if (state.session) {
      state.modeSelected = true;
      applyUiState(state, {
        status: 'REFRESHING SLACK SESSION',
        hint: 'Loading standard emoji and workspace emoji.',
        signedIn: true,
        busy: true,
        loginEnabled: Boolean(config.clientId),
        catalogReady: false,
      });
      state.session = await ensureFreshSession(config, state.session);
      log('session ready', { team: state.session?.team?.name ?? '' });
      await syncCatalog(state);
    } else if (initialRoute.emojiName && config.clientId && !state.autoLoginStarted) {
      state.autoLoginStarted = true;
      applyUiState(state, {
        status: 'SLACK SIGN-IN REQUIRED',
        hint: 'Redirecting to Slack to open this emoji preview.',
        signedIn: false,
        busy: true,
        loginEnabled: true,
        catalogReady: false,
      });
      await startLogin({ popup: false });
    } else {
      log('no session available after boot');
      showModeChoice(state);
    }
  } catch (error) {
    clearSession();
    state.session = null;
    state.workspaceCatalog = { names: [], assetUrls: new Map() };
    showModeChoice(state, 'SLACK SESSION FAILED');
  }

  const scheduleTick = (delayMs) => {
    window.setTimeout(() => {
      window.requestAnimationFrame(() => {
        void tick();
      });
    }, delayMs);
  };

  const tick = async () => {
    try {
      const signOutNonce = wasm.sign_out_request_nonce();
      if (signOutNonce !== state.signOutRequestSeen) {
        state.signOutRequestSeen = signOutNonce;
        if (state.session) {
          signOut();
        }
      }
      state.updateKeypad?.();
      if (!state.session && state.assetUrls.size === 0) {
        if (state.currentEmojiName) {
          state.currentEmojiName = '';
        }
        if (state.loadedEmojiName) {
          state.loadedEmojiName = '';
        }
        scheduleTick(IDLE_POLL_MS);
        return;
      }
      const name = wasm.current_emoji_name();
      syncUrlFromWasm(state);
      if (name !== state.currentEmojiName) {
        log('current emoji changed', { from: state.currentEmojiName, to: name });
        state.currentEmojiName = name;
      }
      if (name !== state.loadedEmojiName) {
        log('emoji asset out of sync', { selected: name, loaded: state.loadedEmojiName });
        await ensureEmojiTexture(state, name);
      }
      preloadPreviewNeighbors(state);
      state.updateKeypad?.();
    } catch (error) {
      console.error('[ultramoji-viewer-4d] tick failed', error);
      if (state.session) {
        applyUiState(state, {
          status: 'EMOJI PREVIEW FETCH FAILED',
          workspace: state.session?.team?.name || '',
          hint: String(error.message || error),
          signedIn: true,
          busy: false,
          loginEnabled: Boolean(config.clientId),
        });
      }
    }
    scheduleTick(ACTIVE_POLL_MS);
  };
  scheduleTick(0);
}

async function syncCatalog(state) {
  if (!state.session) {
    showModeChoice(state);
    return;
  }
  state.modeSelected = true;
  applyUiState(state, {
    status: 'LOADING WORKSPACE EMOJI',
    workspace: state.session?.team?.name || '',
    hint: 'Fetching emoji.list from Slack.',
    signedIn: true,
    busy: true,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: state.standardCatalog.names.length > 0,
  });
  const catalog = await fetchEmojiCatalog(state.session);
  log('emoji catalog loaded', { count: catalog.names.length });
  state.workspaceCatalog = catalog;
  const merged = applyMergedCatalog(state);
  applyRouteToWasm(state);
  applyUiState(state, {
    status: `LOADED ${merged.names.length} EMOJI`,
    workspace: state.session?.team?.name || '',
    hint: '',
    signedIn: true,
    busy: false,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: merged.names.length > 0,
  });
}

async function ensureEmojiTexture(state, name) {
  const requestId = ++state.currentRequestId;
  if (!name) {
    log('clearing emoji texture because name is empty');
    state.wasm.clear_active_emoji_texture();
    state.loadedEmojiName = '';
    return;
  }
  if (state.wasm.set_active_emoji_texture_from_cache(name)) {
    log('using predecoded emoji texture', { name });
    state.failedEmojiNames.delete(name);
    state.decodedEmojiNames.add(name);
    state.loadedEmojiName = name;
    applyPreviewReadyState(state);
    return;
  }
  const urls = emojiAssetUrls(state.assetUrls.get(name));
  if (urls.length === 0) {
    log('no asset url for emoji', { name });
    state.failedEmojiNames.add(name);
    state.wasm.set_active_emoji_texture_error(name);
    state.loadedEmojiName = name;
    return;
  }
  if (state.failedEmojiNames.has(name)) {
    state.wasm.set_active_emoji_texture_error(name);
    state.loadedEmojiName = name;
    return;
  }
  const cached = cachedEmojiBytes(state.assetCache, urls);
  if (cached) {
    const { url, bytes } = cached;
    log('using cached emoji bytes', { name, url });
    if (requestId === state.currentRequestId) {
      const ok = state.wasm.set_active_emoji_texture_bytes(name, bytes);
      log('cached emoji decode handoff', { name, ok });
      if (ok) {
        state.failedEmojiNames.delete(name);
        state.decodedEmojiNames.add(name);
        state.loadedEmojiName = name;
        applyPreviewReadyState(state);
      }
    }
    return;
  }

  applyUiState(state, {
    workspace: state.session?.team?.name || '',
    hint: `Fetching preview bytes for ${urls[0]}`,
    signedIn: Boolean(state.session),
    busy: false,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: state.assetUrls.size > 0,
  });
  let bytes;
  let url;
  state.activeAssetAbortController?.abort();
  const abortController = new AbortController();
  state.activeAssetAbortController = abortController;
  try {
    ({ url, bytes } = await fetchFirstEmojiBytes(urls, abortController.signal));
  } catch (error) {
    if (requestId === state.currentRequestId) {
      state.failedEmojiNames.add(name);
      state.wasm.set_active_emoji_texture_error(name);
      state.loadedEmojiName = name;
    }
    return;
  } finally {
    if (state.activeAssetAbortController === abortController) {
      state.activeAssetAbortController = null;
    }
  }
  state.assetCache.set(url, bytes);
  if (requestId !== state.currentRequestId) {
    log('discarding stale emoji response', { name, url, requestId, currentRequestId: state.currentRequestId });
    return;
  }
  const decoded = state.wasm.set_active_emoji_texture_bytes(name, bytes);
  log('emoji decode handoff', { name, url, decoded });
  if (!decoded) {
    state.failedEmojiNames.add(name);
    state.wasm.set_active_emoji_texture_error(name);
    state.loadedEmojiName = name;
    return;
  }
  state.failedEmojiNames.delete(name);
  state.decodedEmojiNames.add(name);
  state.loadedEmojiName = name;
  applyPreviewReadyState(state);
}

function applyPreviewReadyState(state) {
  applyUiState(state, {
    workspace: state.session?.team?.name || '',
    hint: 'Preview ready.',
    signedIn: Boolean(state.session),
    busy: false,
    loginEnabled: Boolean(state.config.clientId),
    catalogReady: state.assetUrls.size > 0,
  });
}

function preloadPreviewNeighbors(state) {
  if (!state.currentEmojiName) {
    return;
  }
  for (const name of [
    state.wasm.previous_preview_emoji_name(),
    state.wasm.next_preview_emoji_name(),
  ]) {
    if (name) {
      void preloadEmojiTexture(state, name);
    }
  }
}

async function preloadEmojiTexture(state, name) {
  if (
    !name
    || state.decodedEmojiNames.has(name)
    || state.preloadingEmojiNames.has(name)
    || state.failedEmojiNames.has(name)
  ) {
    return;
  }
  const urls = emojiAssetUrls(state.assetUrls.get(name));
  if (urls.length === 0) {
    return;
  }

  state.preloadingEmojiNames.add(name);
  try {
    let bytes;
    let url;
    const cached = cachedEmojiBytes(state.assetCache, urls);
    if (cached) {
      ({ url, bytes } = cached);
    } else {
      ({ url, bytes } = await fetchFirstEmojiBytes(urls));
      state.assetCache.set(url, bytes);
    }
    await waitForIdle();
    const decoded = state.wasm.preload_emoji_texture_bytes(name, bytes);
    log('preloaded emoji texture', { name, decoded });
    if (decoded) {
      state.decodedEmojiNames.add(name);
    }
  } catch (error) {
    log('emoji preload failed', { name, error: String(error?.message || error) });
  } finally {
    state.preloadingEmojiNames.delete(name);
  }
}
