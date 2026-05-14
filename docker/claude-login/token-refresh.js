#!/usr/bin/env node
// Background OAuth token refresh for claude-login container.
//
// Reads credentials.json from CLAUDE_CONFIG_DIR, refreshes the access token
// using the refresh token when it is within REFRESH_BEFORE_EXPIRY_MS of
// expiry, then writes the updated credentials back.  Runs continuously,
// sleeping POLL_INTERVAL_MS between checks.
//
// Why this is needed: agent-runner mounts the credentials volume read-only
// and copies credentials into a per-session dir before spawning claude.
// Claude can refresh its in-session copy, but those refreshed tokens never
// reach the shared volume — so every new session starts from the original,
// eventually-expired credentials.  By keeping the shared copy fresh here
// (in claude-login, which has write access), sessions always start with a
// valid token without relaxing the read-only mount security boundary.

'use strict';

const fs = require('fs');
const https = require('https');
const path = require('path');

const CONFIG_DIR = process.env.CLAUDE_CONFIG_DIR || '/home/loginuser/.claude';
const CREDS_FILE = path.join(CONFIG_DIR, '.credentials.json');
const CREDS_LINK = path.join(CONFIG_DIR, 'credentials.json');
const POLL_INTERVAL_MS = 60 * 60 * 1000;          // check every hour
const REFRESH_BEFORE_EXPIRY_MS = 2 * 60 * 60 * 1000; // refresh when < 2h left
const CLIENT_ID = '9d1c250a-e61b-44d9-88ed-5944d1962f5e';
const TOKEN_ENDPOINT_HOST = 'platform.claude.com';
const TOKEN_ENDPOINT_PATH = '/v1/oauth/token';

function log(msg) {
    process.stdout.write(`[token-refresh] ${new Date().toISOString()} ${msg}\n`);
}

function readCreds() {
    // Resolve the symlink to the actual file so writes hit the real path.
    const realPath = (() => {
        try {
            return fs.realpathSync(CREDS_LINK);
        } catch {
            return CREDS_FILE;
        }
    })();
    if (!fs.existsSync(realPath)) return null;
    try {
        return { data: JSON.parse(fs.readFileSync(realPath, 'utf8')), realPath };
    } catch {
        return null;
    }
}

function postRefresh(refreshToken) {
    return new Promise((resolve, reject) => {
        const body = new URLSearchParams({
            grant_type: 'refresh_token',
            refresh_token: refreshToken,
            client_id: CLIENT_ID,
        }).toString();

        const req = https.request({
            hostname: TOKEN_ENDPOINT_HOST,
            path: TOKEN_ENDPOINT_PATH,
            method: 'POST',
            headers: {
                'Content-Type': 'application/x-www-form-urlencoded',
                'Content-Length': Buffer.byteLength(body),
            },
        }, (res) => {
            let raw = '';
            res.on('data', chunk => { raw += chunk; });
            res.on('end', () => {
                if (res.statusCode === 200) {
                    try {
                        resolve(JSON.parse(raw));
                    } catch (e) {
                        reject(new Error(`Failed to parse token response: ${e.message}`));
                    }
                } else {
                    reject(new Error(`HTTP ${res.statusCode}: ${raw.substring(0, 300)}`));
                }
            });
        });
        req.on('error', reject);
        req.write(body);
        req.end();
    });
}

async function maybeRefresh() {
    const result = readCreds();
    if (!result) {
        log('No credentials file found — waiting for claude /login via SSH.');
        return;
    }
    const { data: creds, realPath } = result;
    const oauth = creds.claudeAiOauth;
    if (!oauth || !oauth.refreshToken) {
        log('No OAuth section in credentials — skipping.');
        return;
    }

    const now = Date.now();
    const remaining = oauth.expiresAt - now;
    if (remaining > REFRESH_BEFORE_EXPIRY_MS) {
        const mins = Math.round(remaining / 60000);
        log(`Token valid for ${mins} min — no refresh needed.`);
        return;
    }

    const expired = remaining <= 0;
    log(expired
        ? `Token expired ${Math.round(-remaining / 60000)} min ago — refreshing.`
        : `Token expires in ${Math.round(remaining / 60000)} min — refreshing early.`);

    let tokens;
    try {
        tokens = await postRefresh(oauth.refreshToken);
    } catch (err) {
        log(`ERROR: Token refresh failed: ${err.message}`);
        log('Re-login required: ssh -p 12222 loginuser@<host> → claude /login');
        return;
    }

    oauth.accessToken = tokens.access_token;
    if (tokens.refresh_token) oauth.refreshToken = tokens.refresh_token;
    oauth.expiresAt = now + (tokens.expires_in * 1000);

    fs.writeFileSync(realPath, JSON.stringify(creds), { mode: 0o600 });
    log(`Token refreshed. New expiry: ${new Date(oauth.expiresAt).toISOString()}`);
}

async function loop() {
    while (true) {
        try {
            await maybeRefresh();
        } catch (err) {
            log(`Unhandled error in refresh loop: ${err.message}`);
        }
        await new Promise(resolve => setTimeout(resolve, POLL_INTERVAL_MS));
    }
}

loop().catch(err => {
    log(`Fatal: ${err.message}`);
    process.exit(1);
});
