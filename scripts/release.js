#!/usr/bin/env node

/**
 * Release Script
 *
 * Handles version bumping, committing, tagging, and pushing to release branch.
 *
 * Usage:
 *   npm run release          - Bump patch version and release
 *   npm run release:patch    - Bump patch version and release
 *   npm run release:minor    - Bump minor version and release
 *   npm run release:major    - Bump major version and release
 */

import fs from 'fs';
import path from 'path';
import { execSync } from 'child_process';
import readline from 'readline';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT_DIR = path.join(__dirname, '..');

const TAURI_CONF_PATH = path.join(ROOT_DIR, 'src-tauri', 'tauri.conf.json');
const CARGO_TOML_PATH = path.join(ROOT_DIR, 'src-tauri', 'Cargo.toml');

const MAIN_BRANCH = 'main';
const RELEASE_BRANCH = 'release';

/**
 * Execute a git command and return output
 */
function git(command, options = {}) {
    const { silent = false, allowFailure = false } = options;
    try {
        const result = execSync(`git ${command}`, {
            cwd: ROOT_DIR,
            encoding: 'utf8',
            stdio: silent ? 'pipe' : 'inherit'
        });
        return result?.trim() || '';
    } catch (error) {
        if (allowFailure) return null;
        throw error;
    }
}

/**
 * Execute git command and return output (always silent)
 */
function gitOutput(command) {
    return execSync(`git ${command}`, {
        cwd: ROOT_DIR,
        encoding: 'utf8'
    }).trim();
}

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
    const status = gitOutput('status --porcelain');
    return status === '';
}

/**
 * Get current git branch
 */
function getCurrentBranch() {
    return gitOutput('branch --show-current');
}

/**
 * Check if a branch exists
 */
function branchExists(branch) {
    const result = git(`show-ref --verify --quiet refs/heads/${branch}`, { silent: true, allowFailure: true });
    return result !== null;
}

/**
 * Wait for user confirmation
 */
function confirm(prompt) {
    return new Promise((resolve) => {
        const rl = readline.createInterface({
            input: process.stdin,
            output: process.stdout
        });

        rl.question(`${prompt} (y/N): `, (answer) => {
            rl.close();
            resolve(answer.toLowerCase() === 'y');
        });
    });
}

/**
 * Main release script
 */
async function main() {
    const bumpType = getBumpType();

    console.log('='.repeat(50));
    console.log('Release Script');
    console.log('='.repeat(50));
    console.log(`Bump type: ${bumpType}\n`);

    // Check we're on main branch
    const currentBranch = getCurrentBranch();
    if (currentBranch !== MAIN_BRANCH) {
        console.error(`Error: Must be on '${MAIN_BRANCH}' branch to release.`);
        console.error(`Current branch: ${currentBranch}`);
        process.exit(1);
    }

    // Check working directory is clean
    if (!isGitClean()) {
        console.error('Error: Git working directory is not clean.');
        console.error('Please commit or stash your changes before releasing.\n');
        git('status --short');
        process.exit(1);
    }

    // Read current version and calculate new version
    const config = readTauriConfig();
    const currentVersion = config.version;
    const newVersion = bumpVersion(currentVersion, bumpType);

    console.log(`Current version: ${currentVersion}`);
    console.log(`New version:     ${newVersion}\n`);

    const proceed = await confirm('Proceed with release?');
    if (!proceed) {
        console.log('Release cancelled.');
        process.exit(0);
    }

    console.log('\n--- Updating version files ---');

    // Update version in both config files
    config.version = newVersion;
    writeTauriConfig(config);
    console.log(`Updated tauri.conf.json to version ${newVersion}`);

    updateCargoTomlVersion(newVersion);
    console.log(`Updated Cargo.toml to version ${newVersion}`);

    console.log('\n--- Committing version bump ---');

    // Stage and commit version files
    git('add src-tauri/tauri.conf.json src-tauri/Cargo.toml');
    git(`commit -m "chore: bump version to ${newVersion}"`);

    // Create tag
    git(`tag -a v${newVersion} -m "Release v${newVersion}"`);
    console.log(`Created tag v${newVersion}`);

    console.log('\n--- Pushing to main ---');
    git(`push origin ${MAIN_BRANCH}`);
    git('push origin --tags');

    console.log('\n--- Updating release branch ---');

    // Create or switch to release branch
    if (branchExists(RELEASE_BRANCH)) {
        git(`checkout ${RELEASE_BRANCH}`);
        git(`rebase ${MAIN_BRANCH}`);
    } else {
        git(`checkout -b ${RELEASE_BRANCH}`);
    }

    // Push release branch
    git(`push origin ${RELEASE_BRANCH} --force`);
    console.log(`Pushed ${RELEASE_BRANCH} branch`);

    // Switch back to main
    console.log('\n--- Switching back to main ---');
    git(`checkout ${MAIN_BRANCH}`);

    console.log('\n' + '='.repeat(50));
    console.log(`Release v${newVersion} initiated!`);
    console.log('GitHub Actions will now build and create the release.');
    console.log('='.repeat(50));
}

main().catch((error) => {
    console.error('Release failed:', error.message);
    process.exit(1);
});
