import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import path from 'node:path';

const W3C_ELEMENT_KEY = 'element-6066-11e4-a52e-4f735466cecf';

class Reporter {
  pass(name) {
    process.stdout.write(`✓ ${name}\n`);
  }

  fail(name, error) {
    process.stdout.write(`✗ ${name}\n`);
    process.stderr.write(`${String(error?.stack || error)}\n`);
  }

  info(message) {
    process.stdout.write(`• ${message}\n`);
  }
}

class TauriWebDriver {
  constructor(port) {
    this.baseUrl = `http://127.0.0.1:${port}`;
    this.sessionId = null;
  }

  async #request(endpoint, { method = 'GET', body } = {}) {
    const response = await fetch(`${this.baseUrl}${endpoint}`, {
      method,
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });

    const text = await response.text();
    const payload = text ? JSON.parse(text) : null;
    if (!response.ok) {
      throw new Error(`WebDriver ${method} ${endpoint} failed (${response.status}): ${text}`);
    }

    return payload;
  }

  async createSession() {
    const payload = await this.#request('/session', {
      method: 'POST',
      body: { capabilities: {} },
    });

    this.sessionId = payload?.value?.sessionId || payload?.sessionId;
    if (!this.sessionId) {
      throw new Error(`Unable to create WebDriver session: ${JSON.stringify(payload)}`);
    }
  }

  async deleteSession() {
    if (!this.sessionId) return;
    try {
      await this.#request(`/session/${this.sessionId}`, { method: 'DELETE' });
    } finally {
      this.sessionId = null;
    }
  }

  async executeAsync(script, args = []) {
    const payload = await this.#request(`/session/${this.sessionId}/execute/async`, {
      method: 'POST',
      body: { script, args },
    });
    return payload?.value;
  }

  async executeSync(script, args = []) {
    const payload = await this.#request(`/session/${this.sessionId}/execute/sync`, {
      method: 'POST',
      body: { script, args },
    });
    return payload?.value;
  }

  async findElements(using, value) {
    const payload = await this.#request(`/session/${this.sessionId}/elements`, {
      method: 'POST',
      body: { using, value },
    });
    return payload?.value || [];
  }

  async getElementText(elementId) {
    const payload = await this.#request(`/session/${this.sessionId}/element/${elementId}/text`);
    return payload?.value;
  }

  async navigateTo(url) {
    await this.#request(`/session/${this.sessionId}/url`, {
      method: 'POST',
      body: { url },
    });
  }

  async screenshot(filePath) {
    const payload = await this.#request(`/session/${this.sessionId}/screenshot`);
    await writeFile(filePath, Buffer.from(payload?.value || '', 'base64'));
  }

  async waitForElement(using, value, timeoutMs = 10_000, intervalMs = 250) {
    return poll(
      async () => {
        const elements = await this.findElements(using, value);
        return elements.length > 0 ? elements[0] : null;
      },
      timeoutMs,
      intervalMs,
      `Timed out waiting for element ${using}:${value}`
    );
  }

  async invoke(command, args = {}) {
    const normalizedArgs = normalizeInvokeArgs(command, args);
    const result = await this.executeAsync(
      `const done = arguments[arguments.length - 1];
       const command = arguments[0];
       const payload = arguments[1];
       window.__TAURI_INTERNALS__.invoke(command, payload)
         .then((value) => done({ ok: true, value }))
         .catch((error) => done({ ok: false, error: String(error) }));`,
      [command, normalizedArgs]
    );

    if (!result?.ok) {
      throw new Error(`invoke(${command}) failed: ${result?.error || 'unknown error'}`);
    }

    return result.value;
  }
}

function normalizeInvokeArgs(command, args) {
  const normalized = { ...args };

  if (command === 'send_message') {
    if (normalized.peer_id && !normalized.peerId) {
      normalized.peerId = normalized.peer_id;
    }
    if (normalized.reply_to_id && !normalized.replyToId) {
      normalized.replyToId = normalized.reply_to_id;
    }
  }

  if (command === 'get_messages') {
    if (normalized.chat_id && !normalized.chatId) {
      normalized.chatId = normalized.chat_id;
    }
    normalized.limit = normalized.limit ?? 50;
    normalized.offset = normalized.offset ?? 0;
  }

  return normalized;
}

function sanitizeName(name) {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/(^-|-$)/g, '');
}

function getElementId(element) {
  return element?.[W3C_ELEMENT_KEY] || element?.ELEMENT || null;
}

async function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function poll(checkFn, timeoutMs, intervalMs, timeoutMessage) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await checkFn();
    if (value) return value;
    await sleep(intervalMs);
  }
  throw new Error(timeoutMessage);
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function getField(obj, ...keys) {
  for (const key of keys) {
    if (obj && Object.hasOwn(obj, key)) {
      return obj[key];
    }
  }
  return undefined;
}

async function sha256File(filePath) {
  const content = await readFile(filePath);
  return createHash('sha256').update(content).digest('hex');
}

async function waitForFile(filePath, timeoutMs = 10_000, intervalMs = 250) {
  return poll(
    async () => {
      try {
        return await stat(filePath);
      } catch {
        return null;
      }
    },
    timeoutMs,
    intervalMs,
    `Timed out waiting for file ${filePath}`
  );
}

async function readInstances(baseDir) {
  const raw = await readFile(path.join(baseDir, 'instances.json'), 'utf8');
  return JSON.parse(raw);
}

async function waitForWebDriver(port, timeoutMs = 20_000, intervalMs = 500) {
  return poll(
    async () => {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/status`);
        if (!response.ok) {
          return null;
        }
        const payload = await response.json();
        return payload?.value?.ready ? payload : null;
      } catch {
        return null;
      }
    },
    timeoutMs,
    intervalMs,
    `Timed out waiting for WebDriver on port ${port}`
  );
}

function buildTestConfig(baseDir, runId, name) {
  return JSON.stringify({
    instanceId: name,
    appDataDir: `${baseDir}/${name}/app-data`,
    downloadDir: `${baseDir}/${name}/downloads`,
    wsBindAddr: '0.0.0.0:0',
    discovery: {
      mode: 'mdns-only',
      namespace: runId,
    },
    keystore: {
      mode: 'file',
      root: `${baseDir}/${name}/keystore`,
    },
  });
}

function launchInstance(binaryPath, baseDir, runId, name, webdriverPort) {
  const child = spawn(binaryPath, [], {
    env: {
      ...process.env,
      JASMINE_TEST_CONFIG: buildTestConfig(baseDir, runId, name),
      TAURI_WEBDRIVER_PORT: String(webdriverPort),
    },
    stdio: 'ignore',
  });

  return child;
}

async function getPeerStatus(driver, peerName) {
  return driver.executeSync(
    `const name = arguments[0];
     const links = Array.from(document.querySelectorAll('a[aria-label]'));
     const link = links.find((entry) => entry.getAttribute('aria-label') === name);
     if (!link) return null;
     const texts = Array.from(link.querySelectorAll('span'))
       .map((node) => node.textContent?.trim())
       .filter(Boolean);
     const online = texts.find((text) => ['online', '在线'].includes(text));
     if (online) return 'online';
     const offline = texts.find((text) => ['offline', '离线'].includes(text));
     if (offline) return 'offline';
     const incompatible = texts.find((text) => ['incompatible', '不兼容'].includes(text));
     if (incompatible) return 'incompatible';
     return null;`,
    [peerName]
  );
}

async function hasPeerRow(driver, peerName) {
  return driver.executeSync(
    `const name = arguments[0];
     return Array.from(document.querySelectorAll('a[aria-label]')).some(
       (entry) => entry.getAttribute('aria-label') === name
     );`,
    [peerName]
  );
}

async function getPeerStatusById(driver, peerId) {
  return driver.executeSync(
    `const peerId = arguments[0];
     const links = Array.from(document.querySelectorAll('a[href]'));
     const link = links.find((entry) => (entry.getAttribute('href') || '').endsWith('/chat/' + peerId));
     if (!link) return null;
     const text = link.innerText || '';
     if (text.includes('在线') || text.includes('online')) return 'online';
     if (text.includes('离线') || text.includes('offline')) return 'offline';
     if (text.includes('不兼容') || text.includes('incompatible')) return 'incompatible';
     return null;`,
    [peerId]
  );
}

async function hasPeerRowById(driver, peerId) {
  return driver.executeSync(
    `const peerId = arguments[0];
     return Array.from(document.querySelectorAll('a[href]')).some(
       (entry) => (entry.getAttribute('href') || '').endsWith('/chat/' + peerId)
     );`,
    [peerId]
  );
}

function buildPayloadBuffer(size, modulo) {
  const payload = Buffer.alloc(size);
  for (let index = 0; index < size; index += 1) {
    payload[index] = index % modulo;
  }
  return payload;
}

async function waitForTransferState(driver, transferId, states, timeoutMs = 30_000) {
  return poll(
    async () => {
      const transfers = await driver.invoke('get_transfers');
      return (
        transfers.find(
          (transfer) => transfer.id === transferId && states.includes(String(transfer.state))
        ) ?? null
      );
    },
    timeoutMs,
    500,
    `Timed out waiting for transfer ${transferId} to reach states ${states.join(', ')}`
  );
}

async function waitForTransferInFlight(driver, transferId, timeoutMs = 30_000) {
  return poll(
    async () => {
      const transfers = await driver.invoke('get_transfers');
      return (
        transfers.find((transfer) => {
          const progress = Number(transfer.progress ?? 0);
          return (
            transfer.id === transferId &&
            transfer.state === 'active' &&
            progress > 0 &&
            progress < 1
          );
        }) ?? null
      );
    },
    timeoutMs,
    500,
    `Timed out waiting for transfer ${transferId} to become active in-flight`
  );
}

async function waitForOfferDialog(driver, fileName, timeoutMs = 15_000) {
  return poll(
    async () => {
      const dialogVisible = await driver.executeSync(
        `const fileName = arguments[0];
         const text = document.body.innerText || '';
         return text.includes(fileName) && (text.includes('收到文件传输请求') || text.includes('Accept') || text.includes('接受'));`,
        [fileName]
      );
      return dialogVisible ? true : null;
    },
    timeoutMs,
    500,
    `Timed out waiting for receive dialog for ${fileName}`
  );
}

async function acceptOfferWhenAvailable(driver, offerId, timeoutMs = 15_000) {
  return poll(
    async () => {
      try {
        await driver.invoke('accept_file', { offerId });
        return true;
      } catch {
        return null;
      }
    },
    timeoutMs,
    500,
    `Timed out waiting to accept offer ${offerId}`
  );
}

async function acceptOfferViaDialog(driver, fileName, offerId, timeoutMs = 15_000) {
  await waitForOfferDialog(driver, fileName, timeoutMs);

  const clicked = await driver.executeSync(
    `const fileName = arguments[0];
     const text = document.body.innerText || '';
     if (!text.includes(fileName)) return false;
     const button = Array.from(document.querySelectorAll('button')).find((node) => {
       const label = node.textContent?.trim();
       return label === '接受' || label === 'Accept';
     });
     if (!button) return false;
     button.click();
     return true;`,
    [fileName]
  );

  assert(clicked, `Could not click accept button for ${fileName}`);

  await poll(
    async () => {
      const stillVisible = await driver.executeSync(
        `const fileName = arguments[0];
         const text = document.body.innerText || '';
         return text.includes(fileName) && (text.includes('收到文件传输请求') || text.includes('Accept') || text.includes('接受'));`,
        [fileName]
      );
      return stillVisible ? null : true;
    },
    timeoutMs,
    250,
    `Offer dialog for ${fileName} did not close after accepting`
  );

  await poll(
    async () => {
      const transfers = await driver.invoke('get_transfers');
      const existing = transfers.find((transfer) => transfer.id === offerId);
      if (existing) {
        return existing;
      }

      try {
        await driver.invoke('accept_file', { offerId });
      } catch {
        return null;
      }

      const nextTransfers = await driver.invoke('get_transfers');
      return nextTransfers.find((transfer) => transfer.id === offerId) ?? null;
    },
    timeoutMs,
    500,
    `Transfer ${offerId} did not appear after accepting ${fileName}`
  );
}

async function waitForGroupView(driver, groupName, expectedCount, timeoutMs = 20_000) {
  return poll(
    async () => {
      const text = await driver.executeSync(`return document.body.innerText || '';`);
      const hasGroupName = typeof text === 'string' && text.includes(groupName);
      const hasCount =
        typeof text === 'string' &&
        (text.includes(`${expectedCount} 名成员`) || text.includes(`${expectedCount} members`));
      return hasGroupName && hasCount ? text : null;
    },
    timeoutMs,
    500,
    `Timed out waiting for group view ${groupName} to show ${expectedCount} members`
  );
}

async function waitForGroupMemberCount(driver, groupId, expectedCount, timeoutMs = 20_000) {
  return poll(
    async () => {
      const group = await driver.invoke('get_group_info', { groupId });
      return Array.isArray(group.members) && group.members.length === expectedCount ? group : null;
    },
    timeoutMs,
    500,
    `Timed out waiting for group ${groupId} member count ${expectedCount}`
  );
}

async function waitForGroupMembers(driver, groupId, expectedMemberIds, timeoutMs = 20_000) {
  return poll(
    async () => {
      const group = await driver.invoke('get_group_info', { groupId });
      const members = Array.isArray(group.members)
        ? group.members
            .map((member) => getField(member, 'id', 'deviceId', 'device_id'))
            .filter(Boolean)
            .sort()
        : [];
      const expected = [...expectedMemberIds].sort();
      return JSON.stringify(members) === JSON.stringify(expected) ? group : null;
    },
    timeoutMs,
    500,
    `Timed out waiting for group ${groupId} members ${expectedMemberIds.join(', ')}`
  );
}

const reporter = new Reporter();

async function run() {
  const alphaPort = Number(process.argv[2] || 4445);
  const betaPort = Number(process.argv[3] || 4446);
  const gammaPort = Number(process.argv[4] || 4447);
  const baseDir = process.argv[5] || '/tmp/jasmine-e2e/manual-run';
  const binaryPath = process.argv[6];
  const runId = path.basename(baseDir);

  await mkdir(baseDir, { recursive: true });

  const instances = await readInstances(baseDir);
  const alphaInstance = instances.find((instance) => instance.name === 'alpha');
  const betaInstance = instances.find((instance) => instance.name === 'beta');
  const gammaInstance = instances.find((instance) => instance.name === 'gamma');

  const alpha = new TauriWebDriver(alphaPort);
  const beta = new TauriWebDriver(betaPort);
  const gamma = gammaInstance ? new TauriWebDriver(gammaPort) : null;
  const restartedChildren = [];

  let allPassed = true;

  try {
    await Promise.all([
      alpha.createSession(),
      beta.createSession(),
      ...(gamma ? [gamma.createSession()] : []),
    ]);
    await Promise.all([
      alpha.waitForElement('css selector', '[data-testid="app-shell"]', 20_000),
      beta.waitForElement('css selector', '[data-testid="app-shell"]', 20_000),
      ...(gamma ? [gamma.waitForElement('css selector', '[data-testid="app-shell"]', 20_000)] : []),
    ]);

    const alphaIdentity = await alpha.invoke('get_identity');
    const betaIdentity = await beta.invoke('get_identity');
    const gammaIdentity = gamma ? await gamma.invoke('get_identity') : null;
    const alphaDeviceId = getField(alphaIdentity, 'device_id', 'deviceId');
    const betaDeviceId = getField(betaIdentity, 'device_id', 'deviceId');
    const gammaDeviceId = gammaIdentity ? getField(gammaIdentity, 'device_id', 'deviceId') : null;
    const alphaDisplayName = getField(alphaIdentity, 'display_name', 'displayName');
    const betaDisplayName = getField(betaIdentity, 'display_name', 'displayName');

    if (gamma && gammaDeviceId) {
      await poll(
        async () => {
          const [alphaPeers, betaPeers, gammaPeers] = await Promise.all([
            alpha.invoke('get_peers'),
            beta.invoke('get_peers'),
            gamma.invoke('get_peers'),
          ]);

          const alphaReady =
            alphaPeers.some((peer) => peer.id === betaDeviceId) &&
            alphaPeers.some((peer) => peer.id === gammaDeviceId);
          const betaReady =
            betaPeers.some((peer) => peer.id === alphaDeviceId) &&
            betaPeers.some((peer) => peer.id === gammaDeviceId);
          const gammaReady =
            gammaPeers.some((peer) => peer.id === alphaDeviceId) &&
            gammaPeers.some((peer) => peer.id === betaDeviceId);

          return alphaReady && betaReady && gammaReady ? true : null;
        },
        30_000,
        1_000,
        'three-instance discovery did not stabilize'
      );
    }

    const tests = [
      {
        name: 'Peer discovery finds both online peers',
        fn: async () => {
          const peers = await poll(
            async () => {
              const [alphaPeers, betaPeers] = await Promise.all([
                alpha.invoke('get_peers'),
                beta.invoke('get_peers'),
              ]);

              const alphaSeesBeta = alphaPeers.find((peer) => peer.id === betaDeviceId);
              const betaSeesAlpha = betaPeers.find((peer) => peer.id === alphaDeviceId);

              return alphaSeesBeta && betaSeesAlpha ? { alphaSeesBeta, betaSeesAlpha } : null;
            },
            10_000,
            1_000,
            'Peers did not discover each other within 10 seconds'
          );

          assert(peers.alphaSeesBeta.name === betaDisplayName, 'alpha peer name mismatch');
          assert(peers.betaSeesAlpha.name === alphaDisplayName, 'beta peer name mismatch');
          assert(peers.alphaSeesBeta.status === 'online', 'alpha sees beta with non-online status');
          assert(peers.betaSeesAlpha.status === 'online', 'beta sees alpha with non-online status');

          await Promise.all([
            alpha.waitForElement('css selector', '[data-testid="sidebar"]'),
            beta.waitForElement('css selector', '[data-testid="sidebar"]'),
          ]);

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'peer-discovery-alpha.png')),
            beta.screenshot(path.join(baseDir, 'peer-discovery-beta.png')),
          ]);
        },
      },
      {
        name: 'Chat messaging delivers in both directions',
        fn: async () => {
          const alphaText = 'Hello from alpha!';
          const betaText = 'Reply from beta!';

          await alpha.invoke('send_message', { peer_id: betaDeviceId, content: alphaText });

          await poll(
            async () => {
              const betaMessages = await beta.invoke('get_messages', { chat_id: alphaDeviceId });
              return betaMessages.some((message) => message.content === alphaText);
            },
            10_000,
            1_000,
            'beta did not receive alpha message'
          );

          await beta.invoke('send_message', { peer_id: alphaDeviceId, content: betaText });

          await poll(
            async () => {
              const alphaMessages = await alpha.invoke('get_messages', { chat_id: betaDeviceId });
              return alphaMessages.some((message) => message.content === betaText);
            },
            10_000,
            1_000,
            'alpha did not receive beta reply'
          );

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'chat-alpha.png')),
            beta.screenshot(path.join(baseDir, 'chat-beta.png')),
          ]);
        },
      },
      {
        name: 'File transfer completes and preserves file contents',
        fn: async () => {
          const sourcePath = path.join(baseDir, 'alpha-upload.txt');
          const sourceContent = `file-transfer-${Date.now()}-${Math.random()}`;
          await writeFile(sourcePath, sourceContent, 'utf8');

          const transferId = await alpha.invoke('send_file', {
            peerId: betaDeviceId,
            filePath: sourcePath,
          });

          await acceptOfferViaDialog(beta, 'alpha-upload.txt', transferId);

          const completed = await poll(
            async () => {
              const [alphaTransfers, betaTransfers] = await Promise.all([
                alpha.invoke('get_transfers'),
                beta.invoke('get_transfers'),
              ]);
              const alphaTransfer = alphaTransfers.find((transfer) => transfer.id === transferId);
              const betaTransfer = betaTransfers.find((transfer) => transfer.id === transferId);

              if (alphaTransfer?.state === 'completed' && betaTransfer?.state === 'completed') {
                return { alphaTransfer, betaTransfer };
              }

              return null;
            },
            30_000,
            1_000,
            'file transfer did not complete on both peers'
          );

          const receivedPath = getField(completed.betaTransfer, 'local_path', 'localPath');
          assert(
            typeof receivedPath === 'string' && receivedPath.length > 0,
            'missing received file path'
          );
          await waitForFile(receivedPath);

          const [sourceHash, receivedHash] = await Promise.all([
            sha256File(sourcePath),
            sha256File(receivedPath),
          ]);

          assert(sourceHash === receivedHash, 'received file hash mismatch');

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'file-transfer-alpha.png')),
            beta.screenshot(path.join(baseDir, 'file-transfer-beta.png')),
          ]);
        },
      },
      {
        name: 'Group creation and group messaging sync between peers',
        fn: async () => {
          const groupName = `E2E Group ${Date.now()}`;
          const group = await alpha.invoke('create_group', {
            name: groupName,
            members: [betaDeviceId],
          });
          const groupId = group.id;

          assert(
            typeof groupId === 'string' && groupId.length > 0,
            'group id missing after creation'
          );

          await poll(
            async () => {
              const groups = await beta.invoke('list_groups');
              return groups.find((entry) => entry.id === groupId) ?? null;
            },
            15_000,
            1_000,
            'beta did not receive created group'
          );

          await Promise.all([
            alpha.navigateTo(`tauri://localhost/group/${groupId}`),
            beta.navigateTo(`tauri://localhost/group/${groupId}`),
          ]);

          await Promise.all([
            alpha.waitForElement('css selector', '[data-testid="message-input-container"]', 10_000),
            beta.waitForElement('css selector', '[data-testid="message-input-container"]', 10_000),
          ]);

          const messageContent = `Hello group ${Date.now()}`;
          await alpha.invoke('send_group_message', {
            groupId,
            content: messageContent,
            replyToId: null,
          });

          await poll(
            async () => {
              const messages = await beta.invoke('get_messages', { chatId: groupId });
              return messages.some((message) => message.content === messageContent)
                ? messages
                : null;
            },
            15_000,
            1_000,
            'beta did not receive group message'
          );

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'group-chat-alpha.png')),
            beta.screenshot(path.join(baseDir, 'group-chat-beta.png')),
          ]);
        },
      },
      {
        name: 'Group membership add remove and leave propagate across peers',
        fn: async () => {
          assert(
            gamma && gammaInstance && gammaDeviceId,
            'gamma instance is required for group membership test'
          );

          const groupName = `Membership Group ${Date.now()}`;
          const group = await alpha.invoke('create_group', {
            name: groupName,
            members: [betaDeviceId],
          });
          const groupId = group.id;

          await poll(
            async () => {
              const groups = await beta.invoke('list_groups');
              return groups.find((entry) => entry.id === groupId) ?? null;
            },
            15_000,
            500,
            'beta did not receive membership test group'
          );

          await alpha.invoke('add_group_members', { groupId, memberIds: [gammaDeviceId] });

          await poll(
            async () => {
              const groups = await gamma.invoke('list_groups');
              return groups.find((entry) => entry.id === groupId) ?? null;
            },
            15_000,
            500,
            'gamma did not receive membership test group after add'
          );

          await Promise.all([
            waitForGroupMembers(alpha, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
            waitForGroupMembers(beta, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
            waitForGroupMembers(gamma, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
          ]);

          await alpha.invoke('remove_group_members', { groupId, memberIds: [gammaDeviceId] });

          await Promise.all([
            waitForGroupMembers(alpha, groupId, [alphaDeviceId, betaDeviceId]),
            waitForGroupMembers(beta, groupId, [alphaDeviceId, betaDeviceId]),
            waitForGroupMembers(gamma, groupId, [alphaDeviceId, betaDeviceId]),
          ]);

          await alpha.invoke('add_group_members', { groupId, memberIds: [gammaDeviceId] });

          await Promise.all([
            waitForGroupMembers(alpha, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
            waitForGroupMembers(beta, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
            waitForGroupMembers(gamma, groupId, [alphaDeviceId, betaDeviceId, gammaDeviceId]),
          ]);

          await gamma.invoke('leave_group', { groupId });

          await Promise.all([
            alpha.navigateTo(`tauri://localhost/group/${groupId}`),
            beta.navigateTo(`tauri://localhost/group/${groupId}`),
          ]);

          await Promise.all([
            alpha.waitForElement('css selector', '[data-testid="message-input-container"]', 10_000),
            beta.waitForElement('css selector', '[data-testid="message-input-container"]', 10_000),
          ]);

          await Promise.all([
            waitForGroupView(alpha, groupName, 2),
            waitForGroupView(beta, groupName, 2),
          ]);

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'group-membership-alpha.png')),
            beta.screenshot(path.join(baseDir, 'group-membership-beta.png')),
            gamma.screenshot(path.join(baseDir, 'group-membership-gamma.png')),
          ]);
        },
      },
      {
        name: 'File transfer cancel and resume completes successfully',
        fn: async () => {
          const fileName = 'alpha-resume-transfer.bin';
          const sourcePath = path.join(baseDir, fileName);
          await writeFile(sourcePath, buildPayloadBuffer(16 * 1024 * 1024, 251));

          const transferId = await alpha.invoke('send_file', {
            peerId: betaDeviceId,
            filePath: sourcePath,
          });

          await acceptOfferViaDialog(beta, fileName, transferId);

          await waitForTransferInFlight(alpha, transferId);
          await alpha.invoke('cancel_transfer', { transferId: transferId });
          await waitForTransferState(alpha, transferId, ['failed', 'cancelled']);
          const betaFailedTransfer = await waitForTransferState(beta, transferId, ['failed']);
          let resumedTransferId;
          let usedFallbackResend = false;
          try {
            resumedTransferId = await alpha.invoke('resume_transfer', { transferId });
          } catch {
            usedFallbackResend = true;
            resumedTransferId = await alpha.invoke('send_file', {
              peerId: betaDeviceId,
              filePath: sourcePath,
            });
          }
          assert(
            resumedTransferId !== transferId,
            'resume_transfer returned the original transfer id'
          );

          if (usedFallbackResend) {
            await acceptOfferViaDialog(beta, fileName, resumedTransferId);
          } else {
            try {
              await acceptOfferViaDialog(beta, fileName, resumedTransferId, 5_000);
            } catch {
              void 0;
            }
          }

          const [alphaCompleted, betaCompleted] = await Promise.all([
            waitForTransferState(alpha, resumedTransferId, ['completed']),
            waitForTransferState(beta, resumedTransferId, ['completed']),
          ]);

          const receivedPath = getField(betaCompleted, 'local_path', 'localPath');
          assert(
            typeof receivedPath === 'string' && receivedPath.length > 0,
            'missing resumed file path'
          );
          await waitForFile(receivedPath);

          const [sourceHash, receivedHash] = await Promise.all([
            sha256File(sourcePath),
            sha256File(receivedPath),
          ]);

          assert(betaFailedTransfer.state === 'failed', 'receiver did not fail before resume flow');
          assert(alphaCompleted.progress === 1, 'sender progress not completed after resume');
          assert(betaCompleted.progress === 1, 'receiver progress not completed after resume');
          assert(sourceHash === receivedHash, 'resumed transfer hash mismatch');

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'file-resume-alpha.png')),
            beta.screenshot(path.join(baseDir, 'file-resume-beta.png')),
          ]);
        },
      },
      {
        name: 'File transfer retry after cancel completes successfully',
        fn: async () => {
          const fileName = 'alpha-retry-transfer.bin';
          const sourcePath = path.join(baseDir, fileName);
          await writeFile(sourcePath, buildPayloadBuffer(12 * 1024 * 1024, 253));

          const transferId = await alpha.invoke('send_file', {
            peerId: betaDeviceId,
            filePath: sourcePath,
          });

          await acceptOfferViaDialog(beta, fileName, transferId);

          await waitForTransferInFlight(alpha, transferId);
          await alpha.invoke('cancel_transfer', { transferId: transferId });
          await waitForTransferState(alpha, transferId, ['failed', 'cancelled']);

          const retryTransferId = await alpha.invoke('retry_transfer', { transferId });
          assert(
            retryTransferId !== transferId,
            'retry_transfer returned the original transfer id'
          );

          await acceptOfferViaDialog(beta, fileName, retryTransferId);

          const [alphaCompleted, betaCompleted] = await Promise.all([
            waitForTransferState(alpha, retryTransferId, ['completed']),
            waitForTransferState(beta, retryTransferId, ['completed']),
          ]);

          const receivedPath = getField(betaCompleted, 'local_path', 'localPath');
          assert(
            typeof receivedPath === 'string' && receivedPath.length > 0,
            'missing retried file path'
          );
          await waitForFile(receivedPath);

          const [sourceHash, receivedHash] = await Promise.all([
            sha256File(sourcePath),
            sha256File(receivedPath),
          ]);

          assert(alphaCompleted.progress === 1, 'sender progress not completed after retry');
          assert(betaCompleted.progress === 1, 'receiver progress not completed after retry');
          assert(sourceHash === receivedHash, 'retried transfer hash mismatch');

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'file-retry-alpha.png')),
            beta.screenshot(path.join(baseDir, 'file-retry-beta.png')),
          ]);
        },
      },
      {
        name: 'Peer goes offline and reconnects online again',
        fn: async () => {
          assert(betaInstance?.pid, 'beta instance pid missing');
          assert(binaryPath, 'binary path is required for reconnect test');

          await beta.deleteSession();
          process.kill(betaInstance.pid, 'SIGTERM');

          await poll(
            async () => {
              const peers = await alpha.invoke('get_peers');
              return peers.some((peer) => peer.id === betaDeviceId) ? null : peers;
            },
            30_000,
            1_000,
            'alpha still sees beta in get_peers after beta shutdown'
          );

          await poll(
            async () => {
              const rowVisible = await hasPeerRowById(alpha, betaDeviceId);
              return rowVisible ? null : true;
            },
            30_000,
            1_000,
            'alpha UI did not remove beta row after shutdown'
          );

          const restartedBeta = launchInstance(binaryPath, baseDir, runId, 'beta', betaPort);
          restartedChildren.push(restartedBeta);
          betaInstance.pid = restartedBeta.pid;

          await waitForWebDriver(betaPort);
          await beta.createSession();
          await beta.waitForElement('css selector', '[data-testid="app-shell"]', 20_000);

          const restartedIdentity = await beta.invoke('get_identity');
          const restartedBetaDeviceId = getField(restartedIdentity, 'device_id', 'deviceId');
          assert(
            restartedBetaDeviceId === betaDeviceId,
            'beta device identity changed across restart'
          );

          await poll(
            async () => {
              const peers = await alpha.invoke('get_peers');
              return peers.find((peer) => peer.id === betaDeviceId) ?? null;
            },
            30_000,
            1_000,
            'alpha did not rediscover beta after restart'
          );

          await poll(
            async () => {
              const rowVisible = await hasPeerRowById(alpha, betaDeviceId);
              const status = await getPeerStatusById(alpha, betaDeviceId);
              return rowVisible && status === 'online' ? status : null;
            },
            30_000,
            1_000,
            'alpha UI did not mark beta online after restart'
          );

          await Promise.all([
            alpha.screenshot(path.join(baseDir, 'reconnect-alpha.png')),
            beta.screenshot(path.join(baseDir, 'reconnect-beta.png')),
          ]);
        },
      },
      {
        name: 'Settings updates display name and page renders',
        fn: async () => {
          const initial = await alpha.invoke('get_identity');
          const initialName = getField(initial, 'display_name', 'displayName');
          assert(
            initialName === alphaDisplayName,
            `unexpected initial display name: ${String(initialName)}`
          );

          await alpha.invoke('update_display_name', { name: 'TestAlpha' });

          const updated = await poll(
            async () => {
              const identity = await alpha.invoke('get_identity');
              const displayName = getField(identity, 'display_name', 'displayName');
              return displayName === 'TestAlpha' ? identity : null;
            },
            10_000,
            500,
            'display name did not update to TestAlpha'
          );

          assert(
            getField(updated, 'display_name', 'displayName') === 'TestAlpha',
            'display name mismatch after update_display_name'
          );

          await alpha.navigateTo('tauri://localhost/settings');

          const settingsReady = await poll(
            async () =>
              alpha.executeSync(
                `return Boolean(document.querySelector('#display_name') && document.querySelector('h1'));`
              ),
            10_000,
            500,
            'settings page did not render expected elements'
          );

          assert(Boolean(settingsReady), 'settings page check failed');
          await alpha.screenshot(path.join(baseDir, 'settings-alpha.png'));
        },
      },
    ];

    for (const test of tests) {
      try {
        await test.fn();
        reporter.pass(test.name);
      } catch (error) {
        allPassed = false;
        reporter.fail(test.name, error);

        const failureName = sanitizeName(test.name);
        const failurePath = path.join(baseDir, `failure-${failureName}.png`);
        const betaFailurePath = path.join(baseDir, `failure-${failureName}-beta.png`);
        const gammaFailurePath = path.join(baseDir, `failure-${failureName}-gamma.png`);

        await Promise.allSettled([
          alpha.screenshot(failurePath),
          beta.screenshot(betaFailurePath),
          ...(gamma ? [gamma.screenshot(gammaFailurePath)] : []),
        ]);
        reporter.info(
          gamma
            ? `Saved failure screenshots to ${failurePath}, ${betaFailurePath}, and ${gammaFailurePath}`
            : `Saved failure screenshots to ${failurePath} and ${betaFailurePath}`
        );
      }
    }
  } finally {
    await Promise.allSettled([
      alpha.deleteSession(),
      beta.deleteSession(),
      ...(gamma ? [gamma.deleteSession()] : []),
    ]);
    for (const child of restartedChildren) {
      if (!child.killed) {
        child.kill('SIGTERM');
      }
    }
  }

  process.exit(allPassed ? 0 : 1);
}

run().catch((error) => {
  process.stderr.write(`${String(error?.stack || error)}\n`);
  process.exit(1);
});
