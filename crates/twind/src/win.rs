//! The few Win32 calls more than one module needs.

use std::ffi::OsStr;
use std::io;
use std::iter;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED, GetLastError, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, LookupAccountNameW, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, SID_NAME_USE, SidTypeAlias, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(iter::once(0)).collect()
}

/// A security descriptor Windows built from SDDL, freed when dropped.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor = null_mut();
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1, &mut descriptor, null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    pub fn as_raw(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    /// Valid only while `self` is alive.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: self.0, bInheritHandle: 0 }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

pub fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut needed = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed) };
    // u64s, so the buffer is aligned for the pointers inside TOKEN_USER.
    let mut buffer = vec![0u64; needed as usize / 8 + 1];
    let size = (buffer.len() * 8) as u32;
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), size, &mut needed) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_to_string(user.User.Sid)
}

fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

/// The account's SID and kind, or `None` if no such account exists.
pub fn account_sid(account: &str) -> io::Result<Option<(String, SID_NAME_USE)>> {
    let name = wide(account);
    let (mut sid_size, mut domain_size, mut kind) = (0u32, 0u32, 0);
    unsafe { LookupAccountNameW(null(), name.as_ptr(), null_mut(), &mut sid_size, null_mut(), &mut domain_size, &mut kind) };
    match unsafe { GetLastError() } {
        ERROR_NONE_MAPPED => return Ok(None),
        ERROR_INSUFFICIENT_BUFFER => {}
        code => return Err(io::Error::from_raw_os_error(code as i32)),
    }
    let mut sid = vec![0u32; (sid_size as usize).div_ceil(4)];
    let mut domain = vec![0u16; domain_size as usize];
    let ok = unsafe { LookupAccountNameW(null(), name.as_ptr(), sid.as_mut_ptr().cast(), &mut sid_size, domain.as_mut_ptr(), &mut domain_size, &mut kind) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some((sid_to_string(sid.as_mut_ptr().cast())?, kind)))
}

/// The SID of a group on this machine. The name is qualified with the machine's, so a domain
/// group that happens to share the name can't be picked up instead.
pub fn local_group_sid(group: &str) -> io::Result<Option<String>> {
    let mut buffer = [0u16; 64];
    let mut length = buffer.len() as u32;
    if unsafe { GetComputerNameW(buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let computer = String::from_utf16_lossy(&buffer[..length as usize]);
    Ok(account_sid(&format!(r"{computer}\{group}"))?.filter(|(_, kind)| *kind == SidTypeAlias).map(|(sid, _)| sid))
}

pub fn is_admin() -> io::Result<bool> {
    let mut sid = [0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)];
    let mut size = SECURITY_MAX_SID_SIZE;
    if unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, null_mut(), sid.as_mut_ptr().cast(), &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut member = 0;
    if unsafe { CheckTokenMembership(null_mut(), sid.as_mut_ptr().cast(), &mut member) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(member != 0)
}

pub fn random_u64() -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    let status = unsafe { BCryptGenRandom(null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status < 0 {
        return Err(io::Error::other(format!("the system random generator failed with status {status:#x}")));
    }
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_user_sid() {
        assert!(current_user_sid().unwrap().starts_with("S-1-"));
    }

    #[test]
    fn a_missing_group_resolves_to_nothing() {
        assert_eq!(local_group_sid("SenseTwin Group That Was Never Created").unwrap(), None);
    }

    #[test]
    fn a_bad_descriptor_is_refused() {
        assert!(SecurityDescriptor::from_sddl("not sddl at all").is_err());
    }

    #[test]
    fn random_values_differ() {
        assert_ne!(random_u64().unwrap(), random_u64().unwrap());
    }
}
