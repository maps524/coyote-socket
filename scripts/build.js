#!/usr/bin/env node

/**
 * Tauri Build Script
 *
 * Builds the Tauri app and optionally renames the exe with version number.
 *
 * Usage:
 *   npm run tauri:build              - Build without renaming
 *   npm run tauri:build -- --rename  - Build and rename exe with version
 */

import fs from 'fs';
import { spawn } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT_DIR = path.join(__dirname, '..');

const CARGO_TOML_PATH = path.join(ROOT_DIR, 'src-tauri', 'Cargo.toml');
const RELEASE_DIR = path.join(ROOT_DIR, 'src-tauri', 'target', 'release');

/**
 * Check if --rename flag is passed
 */
function shouldRename() {
    return process.argv.includes('--rename');
}

/**
 * Get version from Cargo.toml
 */
function getVersion() {
    const content = fs.readFileSync(CARGO_TOML_PATH, 'utf8');
    const match = content.match(/^version\s*=\s*"([\d.]+)"/m);
    if (!match) {
        throw new Error('Could not find version in Cargo.toml');
    }
    return match[1];
}

/**
 * Rename the built executable to include version number
 */
function renameExecutable(version) {
    const originalExe = path.join(RELEASE_DIR, 'coyote-socket.exe');
    const versionedExe = path.join(RELEASE_DIR, `CoyoteSocket-${version}.exe`);

    if (!fs.existsSync(originalExe)) {
        console.warn('Warning: Could not find built executable to rename');
        return null;
    }

    // Remove old versioned exe if it exists
    if (fs.existsSync(versionedExe)) {
        fs.unlinkSync(versionedExe);
    }

    fs.copyFileSync(originalExe, versionedExe);
    console.log(`Created versioned executable: CoyoteSocket-${version}.exe`);
    return versionedExe;
}

/**
 * Run the Tauri build
 */
function runTauriBuild() {
    return new Promise((resolve, reject) => {
        console.log('Starting Tauri build...\n');

        const isWindows = process.platform === 'win32';
        const npm = isWindows ? 'npm.cmd' : 'npm';

        const build = spawn(npm, ['run', 'tauri', 'build'], {
            cwd: ROOT_DIR,
            stdio: 'inherit',
            shell: true
        });

        build.on('close', (code) => {
            if (code === 0) {
                resolve();
            } else {
                reject(new Error(`Build failed with exit code ${code}`));
            }
        });

        build.on('error', (error) => {
            reject(error);
        });
    });
}

async function main() {
    const rename = shouldRename();

    try {
        await runTauriBuild();

        if (rename) {
            const version = getVersion();
            renameExecutable(version);
        }

        console.log('\nBuild complete!');
    } catch (error) {
        console.error('\nBuild failed:', error.message);
        process.exit(1);
    }
}

main();
