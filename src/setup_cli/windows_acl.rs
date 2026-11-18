//! Windows protection checks for existing private setup storage, made through direct
//! system calls.
//!
//! This mirrors the verification modes of the fixed PowerShell check in the parent module
//! (`directory`, `file`, `directory-file` and `input-directory`): the same owner, access-rule
//! and ancestor conditions, in the same order, reporting the same failure fields. Creating a
//! private directory stays with the PowerShell helper.
//!
//! This is the only module that calls the operating system directly, so it is the only one
//! where `unsafe` is allowed. Every block states why its call is sound.
#![allow(unsafe_code)]

use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_NAME, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
        GENERIC_ALL, GENERIC_WRITE, GetLastError, HANDLE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT},
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetTokenInformation,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, GetFileAttributesW,
        INVALID_FILE_ATTRIBUTES,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// One refused path. Empty fields produce the parent's unbounded-diagnostic message.
pub(super) struct Failure {
    pub(super) path: String,
    pub(super) sid: String,
    pub(super) right: String,
    pub(super) reason: &'static str,
}

const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";
const CREATOR_OWNER: &str = "S-1-3-0";
const EVERYONE: &str = "S-1-1-0";
/// `NT SERVICE\TrustedInstaller`. A service identifier is derived from the service name, so
/// it is the same on every installation.
const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;
const OBJECT_INHERIT: u8 = 0x01;
const CONTAINER_INHERIT: u8 = 0x02;
const INHERIT_ONLY: u8 = 0x08;

/// Delete, DeleteSubdirectoriesAndFiles, ChangePermissions and TakeOwnership.
const DANGER: u32 = 0x0001_0000 | 0x0000_0040 | 0x0004_0000 | 0x0008_0000;
const FULL_CONTROL: u32 = 0x001F_01FF;

/// File-system right names in ascending value order, as the PowerShell check printed them.
const RIGHTS: [(u32, &str); 19] = [
    (0x0000_0001, "ReadData"),
    (0x0000_0002, "CreateFiles"),
    (0x0000_0004, "AppendData"),
    (0x0000_0008, "ReadExtendedAttributes"),
    (0x0000_0010, "WriteExtendedAttributes"),
    (0x0000_0020, "ExecuteFile"),
    (0x0000_0040, "DeleteSubdirectoriesAndFiles"),
    (0x0000_0080, "ReadAttributes"),
    (0x0000_0100, "WriteAttributes"),
    (0x0000_0116, "Write"),
    (0x0001_0000, "Delete"),
    (0x0002_0000, "ReadPermissions"),
    (0x0002_0089, "Read"),
    (0x0002_00A9, "ReadAndExecute"),
    (0x0003_01BF, "Modify"),
    (0x0004_0000, "ChangePermissions"),
    (0x0008_0000, "TakeOwnership"),
    (0x0010_0000, "Synchronize"),
    (FULL_CONTROL, "FullControl"),
];

struct Rule {
    allow: bool,
    inherit_only: bool,
    mask: u32,
    sid: String,
}

/// Owner and access list of one path. Both pointers refer into `descriptor`.
struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}
impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW and is freed once.
        unsafe { LocalFree(self.descriptor) };
    }
}

pub(super) fn check(path: &Path, mode: &str, parent: Option<&Path>) -> Result<(), Failure> {
    let user = current_user().ok_or_else(unreported)?;
    let trusted = [
        user.as_str(),
        SYSTEM,
        ADMINISTRATORS,
        CREATOR_OWNER,
        TRUSTED_INSTALLER,
    ];
    let combined = mode == "directory-file";
    let (target, file) = if combined || mode == "input-directory" {
        (parent.ok_or_else(unreported)?, false)
    } else {
        (path, mode == "file")
    };
    if !file {
        check_ancestors(target, &trusted)?;
    }
    check_private(target, file, &user)?;
    if !file {
        check_ancestors(target, &trusted)?;
    }
    if combined {
        check_private(path, true, &user)?;
    }
    Ok(())
}

fn check_ancestors(path: &Path, trusted: &[&str]) -> Result<(), Failure> {
    let absolute = std::path::absolute(path).map_err(|_| inspect(path))?;
    for directory in absolute.ancestors().skip(1) {
        let attributes = match attributes(directory) {
            Ok(attributes) => attributes,
            Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => continue,
            Err(_) => return Err(inspect(directory)),
        };
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(fail(directory, "unknown", "ReparsePoint", "reparse-point"));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(fail(directory, "unknown", "Directory", "not-directory"));
        }
        let security = security(directory).ok_or_else(|| inspect(directory))?;
        let owner = sid_string(security.owner).unwrap_or_default();
        if !trusted.contains(&owner.as_str()) {
            return Err(fail(directory, &owner, "Owner", "owner"));
        }
        let rules = rules(security.dacl, true, false).ok_or_else(|| inspect(directory))?;
        if rules.is_empty() {
            return Err(fail(directory, "unknown", "AccessRules", "empty-acl"));
        }
        for rule in &rules {
            let rights = rule.mask & DANGER;
            if rule.allow
                && !rule.inherit_only
                && rights != 0
                && !trusted.contains(&rule.sid.as_str())
            {
                return Err(fail(directory, &rule.sid, &rights_text(rights), "allow"));
            }
        }
    }
    Ok(())
}

fn check_private(path: &Path, file: bool, user: &str) -> Result<(), Failure> {
    let attributes = attributes(path).map_err(|_| inspect(path))?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(fail(path, "unknown", "ReparsePoint", "reparse-point"));
    }
    let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if file && directory {
        return Err(fail(path, "unknown", "RegularFile", "not-file"));
    }
    if !file && !directory {
        return Err(fail(path, "unknown", "Directory", "not-directory"));
    }
    let security = security(path).ok_or_else(|| inspect(path))?;
    let owner = sid_string(security.owner).unwrap_or_default();
    if owner != user {
        return Err(fail(path, &owner, "Owner", "owner"));
    }
    let mut own = false;
    for rule in rules(security.dacl, !file, false).ok_or_else(|| inspect(path))? {
        if !rule.allow {
            continue;
        }
        if rule.sid != user && rule.sid != SYSTEM {
            return Err(fail(path, &rule.sid, &rights_text(rule.mask), "allow"));
        }
        own |= rule.sid == user;
    }
    if !own {
        return Err(fail(path, user, "AccessRules", "missing-self"));
    }
    Ok(())
}

/// Whether this settings path belongs to the user and whether Everyone can write it.
pub(crate) fn hook_permissions(path: &Path, directory: bool, user: &str) -> Option<(bool, bool)> {
    let security = security(path)?;
    if sid_string(security.owner)? != user {
        return Some((false, false));
    }
    let change = DANGER
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | GENERIC_WRITE
        | GENERIC_ALL;
    let shared = rules(security.dacl, directory, true)?.iter().any(|rule| {
        rule.allow && !rule.inherit_only && rule.sid == EVERYONE && rule.mask & change != 0
    });
    Some((true, shared))
}

fn fail(path: &Path, sid: &str, right: &str, reason: &'static str) -> Failure {
    Failure {
        path: path.to_string_lossy().into_owned(),
        sid: sid.into(),
        right: right.into(),
        reason,
    }
}

fn inspect(path: &Path) -> Failure {
    fail(path, "unknown", "AclInspection", "inspect")
}

fn unreported() -> Failure {
    fail(Path::new(""), "", "", "")
}

/// Renders an access mask the way the PowerShell check printed `FileSystemRights`: the
/// largest named values first, joined in ascending order, or the signed number when bits
/// outside the named values remain.
fn rights_text(mask: u32) -> String {
    let mut rest = mask;
    let mut names = Vec::new();
    for (value, name) in RIGHTS.iter().rev() {
        if rest & value == *value {
            rest -= value;
            names.push(*name);
        }
    }
    if rest != 0 || names.is_empty() {
        return (mask as i32).to_string();
    }
    names.reverse();
    names.join(", ")
}

fn wide(path: &Path) -> Option<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return None;
    }
    wide.push(0);
    Some(wide)
}

/// File attributes without following a final reparse point, or the system error code.
fn attributes(path: &Path) -> Result<u32, u32> {
    let wide = wide(path).ok_or(ERROR_INVALID_NAME)?;
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        // SAFETY: reads the calling thread's last error code.
        Err(unsafe { GetLastError() })
    } else {
        Ok(attributes)
    }
}

fn security(path: &Path) -> Option<Security> {
    let wide = wide(path)?;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string and every out pointer refers to a
    // live local. On success the system allocates `descriptor`, which `Security` frees.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if descriptor.is_null() {
        return None;
    }
    let security = Security {
        descriptor,
        owner,
        dacl,
    };
    (status == ERROR_SUCCESS).then_some(security)
}

/// The access rules of a list, without entries that can never grant access: an empty mask,
/// an inherit-only entry on a file, or an inherit-only entry that nothing inherits.
fn rules(dacl: *const ACL, container: bool, strict: bool) -> Option<Vec<Rule>> {
    if dacl.is_null() {
        // An object without an access list grants everyone full access.
        return Some(vec![Rule {
            allow: true,
            inherit_only: false,
            mask: FULL_CONTROL,
            sid: EVERYONE.into(),
        }]);
    }
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` points into a live security descriptor and `size` is a writable
    // ACL_SIZE_INFORMATION of the stated length.
    let known = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if known == 0 {
        return None;
    }
    let mut rules = Vec::new();
    for index in 0..size.AceCount {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: `dacl` is live and `index` is below its entry count.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return None;
        }
        // SAFETY: GetAce returned an entry inside the list; every entry starts with a header.
        let header = unsafe { ace.cast::<ACE_HEADER>().read_unaligned() };
        if header.AceType != ACCESS_ALLOWED && header.AceType != ACCESS_DENIED {
            if strict {
                return None;
            }
            continue;
        }
        if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
            return None;
        }
        let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: allowed and denied entries share this layout, and the size check above
        // covers the mask and the start of the identifier.
        let (mask, sid) = unsafe {
            (
                (&raw const (*entry).Mask).read_unaligned(),
                (&raw mut (*entry).SidStart).cast::<c_void>(),
            )
        };
        let inherit_only = header.AceFlags & INHERIT_ONLY != 0;
        let inherited_by_children = header.AceFlags & (CONTAINER_INHERIT | OBJECT_INHERIT) != 0;
        if mask == 0 || (inherit_only && !(container && inherited_by_children)) {
            continue;
        }
        rules.push(Rule {
            allow: header.AceType == ACCESS_ALLOWED,
            inherit_only,
            mask,
            sid: sid_string(sid)?,
        });
    }
    Some(rules)
}

fn sid_string(sid: PSID) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points to an identifier inside a live descriptor or token buffer. On
    // success the system allocates `text`, which is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return None;
    }
    // SAFETY: `text` is a NUL-terminated UTF-16 string; the bound stops a runaway read.
    let value = unsafe {
        let mut length = 0;
        while length < 256 && *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(text, length)).ok()
    };
    // SAFETY: `text` was allocated by ConvertSidToStringSidW and is freed once.
    unsafe { LocalFree(text.cast()) };
    value
}

/// The identifier of the user this process runs as.
pub(crate) fn current_user() -> Option<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle for this process is always valid; `token` is a live local.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut needed = 0u32;
    // SAFETY: a zero-length query only reports the required size.
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed) };
    // Eight-byte elements keep the pointer inside TOKEN_USER aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(2)];
    // SAFETY: `buffer` is writable for the stated number of bytes and `token` is open.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    // SAFETY: `token` was opened above and is closed once.
    unsafe { CloseHandle(token) };
    if read == 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose identifier pointer
    // refers into the same buffer, which is still alive.
    sid_string(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

#[cfg(test)]
mod tests {
    use super::rights_text;

    #[test]
    fn rights_are_named_like_the_fixed_helper() {
        for (mask, text) in [
            (0x001F_01FF, "FullControl"),
            (0x0012_0089, "Read, Synchronize"),
            (0x0012_00A9, "ReadAndExecute, Synchronize"),
            (0x0013_01BF, "Modify, Synchronize"),
            (
                0x000D_0040,
                "DeleteSubdirectoriesAndFiles, Delete, ChangePermissions, TakeOwnership",
            ),
            (0x0001_0000, "Delete"),
            (0x0000_0006, "CreateFiles, AppendData"),
            (0x1000_0000, "268435456"),
            (0xA000_0000, "-1610612736"),
            (0, "0"),
        ] {
            assert_eq!(rights_text(mask), text, "{mask:#x}");
        }
    }
}
