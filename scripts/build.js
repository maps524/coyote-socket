#!/usr/bin/env node

/**
 * Tauri Build Script with Version Bumping
 *
 * Usage:
 *   npm run tauri:build          - Bump patch version and build
 *   npm run tauri:build:patch    - Bump patch version and build
 *   npm run tauri:build:minor    - Bump minor version and build
 *   npm run tauri:build:major    - Bump major version and build
 */

import fs from 'fs';
import path from 'path';
import { execSync, spawn } from 'child_process';
import readline from 'readline';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const TAURI_CONF_PATH = path.join(__dirname, '..', 'src-tauri', 'tauri.conf.json');
const CARGO_TOML_PATH = path.join(__dirname, '..', 'src-tauri', 'Cargo.toml');

/**
 * Parse command line arguments to determine bump type
 */
function getBumpType() {
    const args = process.argv.slice(2);
    const bumpArg = args.find(arg => ['patch', 'minor', 'major'].includes(arg));
    return bumpArg || 'patch';
}

/**
 * Read and parse tauri.conf.json
 */
function readTauriConfig() {
    const content = fs.readFileSync(TAURI_CONF_PATH, 'utf8');
    return JSON.parse(content);
}

/**
 * Write tauri.conf.json with proper formatting
 */
function writeTauriConfig(config) {
    const content = JSON.stringify(config, null, 2) + '\n';
    fs.writeFileSync(TAURI_CONF_PATH, content, 'utf8');
}

/**
 * Read Cargo.toml content
 */
function readCargoToml() {
    return fs.readFileSync(CARGO_TOML_PATH, 'utf8');
}

/**
 * Update version in Cargo.toml
 */
function updateCargoTomlVersion(newVersion) {
    let content = readCargoToml();
    // Match version = "x.y.z" in the [package] section
    content = content.replace(
        /^(version\s*=\s*")[\d.]+(")/m,
        `$1${newVersion}$2`
    );
    fs.writeFileSync(CARGO_TOML_PATH, content, 'utf8');
}

/**
 * Increment version based on bump type
 */
function bumpVersion(currentVersion, bumpType) {
    const parts = currentVersion.split('.').map(Number);

    if (parts.length !== 3 || parts.some(isNaN)) {
        throw new Error(`Invalid version format: ${currentVersion}`);
    }

    let [major, minor, patch] = parts;

    switch (bumpType) {
        case 'major':
            major++;
            minor = 0;
            patch = 0;
            break;
        case 'minor':
            minor++;
            patch = 0;
            break;
        case 'patch':
        default:
            patch++;
            break;
    }

    return `${major}.${minor}.${patch}`;
}

/**
 * Check if git working directory is clean
 */
function isGitClean() {
    try {
        const status = execSync('git status --porcelain', {
            encoding: 'utf8',
            cwd: path.join(__dirname, '..')
        });
        return status.trim() === '';
    } catch (error) {
        console.error('Error checking git status:', error.message);
        return false;
    }
}

/**
 * Get current git status for display
 */
function getGitStatus() {
    try {
        return execSync('git status --short', {
            encoding: 'utf8',
            cwd: path.join(__dirname, '..')
        });
    } catch (error) {
        return 'Unable to get git status';
    }
}

/**
 * Commit version bump and create tag
 */
function commitAndTag(version) {
    const cwd = path.join(__dirname, '..');

    try {
        // Stage both version files
        execSync('git add src-tauri/tauri.conf.json src-tauri/Cargo.toml', { cwd, stdio: 'inherit' });

        // Commit with version message
        execSync(`git commit -m "chore: bump version to ${version}"`, { cwd, stdio: 'inherit' });

        // Create annotated tag
        execSync(`git tag -a v${version} -m "Release v${version}"`, { cwd, stdio: 'inherit' });

        console.log(`\nCommitted and tagged as v${version}`);
        return true;
    } catch (error) {
        console.error('Error committing/tagging:', error.message);
        return false;
    }
}

/**
 * Wait for user to press Enter
 */
function waitForEnter(prompt) {
    return new Promise((resolve) => {
        const rl = readline.createInterface({
            input: process.stdin,
            output: process.stdout
        });

        rl.question(prompt, () => {
            rl.close();
            resolve();
        });
    });
}

/**
 * Rename the built executable to include version number
 */
function renameExecutable(version) {
    const releaseDir = path.join(__dirname, '..', 'src-tauri', 'target', 'release');
    const originalExe = path.join(releaseDir, 'coyote-socket.exe');
    const versionedExe = path.join(releaseDir, `CoyoteSocket-${version}.exe`);

    if (fs.existsSync(originalExe)) {
        // Remove old versioned exe if it exists
        if (fs.existsSync(versionedExe)) {
            fs.unlinkSync(versionedExe);
        }
        fs.copyFileSync(originalExe, versionedExe);
        console.log(`\nCreated versioned executable: CoyoteSocket-${version}.exe`);
        return versionedExe;
    } else {
        console.warn('\nWarning: Could not find built executable to rename');
        return null;
    }
}

/**
 * Run the Tauri build
 */
function runTauriBuild() {
    return new Promise((resolve, reject) => {
        console.log('\nStarting Tauri build...\n');

        const isWindows = process.platform === 'win32';
        const npm = isWindows ? 'npm.cmd' : 'npm';

        const build = spawn(npm, ['run', 'tauri', 'build'], {
            cwd: path.join(__dirname, '..'),
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

/**
 * Main build script
 */
async function main() {
    const bumpType = getBumpType();

    console.log('='.repeat(50));
    console.log('Tauri Build with Version Bump');
    console.log('='.repeat(50));
    console.log(`Bump type: ${bumpType}\n`);

    // Read current version
    const config = readTauriConfig();
    const currentVersion = config.version;
    const newVersion = bumpVersion(currentVersion, bumpType);

    console.log(`Current version: ${currentVersion}`);
    console.log(`New version:     ${newVersion}\n`);

    // Check if git is clean before bumping
    while (!isGitClean()) {
        console.log('Git working directory is not clean:\n');
        console.log(getGitStatus());
        console.log('\nPlease commit or stash your changes before proceeding.');
        await waitForEnter('Press Enter to check again...');
        console.log(''); // Empty line for readability
    }

    console.log('Git working directory is clean.\n');

    // Update version in both config files
    config.version = newVersion;
    writeTauriConfig(config);
    console.log(`Updated tauri.conf.json to version ${newVersion}`);

    updateCargoTomlVersion(newVersion);
    console.log(`Updated Cargo.toml to version ${newVersion}`);

    // Commit and tag
    if (!commitAndTag(newVersion)) {
        console.error('\nFailed to commit and tag. Aborting build.');
        process.exit(1);
    }

    // Run the build
    try {
        await runTauriBuild();

        // Create versioned executable
        const versionedExe = renameExecutable(newVersion);

        console.log('\n' + '='.repeat(50));
        console.log(`Build complete! Version: v${newVersion}`);
        if (versionedExe) {
            console.log(`Output: ${path.basename(versionedExe)}`);
        }
        console.log('='.repeat(50));
    } catch (error) {
        console.error('\nBuild failed:', error.message);
        console.log('\nNote: Version was already bumped and committed.');
        console.log('You may want to revert the commit if the build issue is not fixable.');
        process.exit(1);
    }
}

main().catch((error) => {
    console.error('Unexpected error:', error);
    process.exit(1);
});
