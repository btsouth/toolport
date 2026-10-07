//! Private recovery directories also protect atomic-write temps on Windows.
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, GENERIC_ALL};
use windows_sys::Win32::Security::Authorization::{
    SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, SET_ACCESS, SE_FILE_OBJECT,
    TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub(super) fn secure(dir: &Path) -> Result<(), String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: valid output handles/buffers; every owned allocation/handle is
    // released below. The SID remains in the aligned buffer until ACL creation.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let user = (|| {
            let mut size = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
            if size == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(buffer)
        })();
        CloseHandle(token);
        let user = user?;
        let sid = (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid.cast(),
                ..Default::default()
            },
        };
        let mut acl = std::ptr::null_mut();
        let result = SetEntriesInAclW(1, &access, std::ptr::null(), &mut acl);
        if result != 0 {
            return Err(std::io::Error::from_raw_os_error(result as i32).to_string());
        }
        let mut path: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = SetNamedSecurityInfoW(
            path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        );
        LocalFree(acl.cast());
        if result != 0 {
            return Err(std::io::Error::from_raw_os_error(result as i32).to_string());
        }
    }
    Ok(())
}
