//! Shared Windows protection for native credential and CLI response files.
//! Retains the existing native-host error contract and verifies the applied DACL.
use crate::dsh_application::{DshApplicationError, DshApplicationResult};
use std::path::Path;
fn internal(message: impl Into<String>) -> DshApplicationError {
    DshApplicationError::internal(message)
}

#[cfg(windows)]
pub fn set_private_permissions(path: &Path) -> DshApplicationResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GENERIC_ALL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
        TokenUser, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let _token = HandleGuard(token);
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(internal("Windows did not return the current user SID"));
    }
    let word = std::mem::size_of::<usize>();
    let mut user_storage = vec![0usize; (needed as usize).div_ceil(word)];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user_storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let user_sid = unsafe { (*(user_storage.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut system_storage = vec![0usize; (SECURITY_MAX_SID_SIZE as usize).div_ceil(word)];
    let mut system_len = SECURITY_MAX_SID_SIZE;
    let system_sid: PSID = system_storage.as_mut_ptr().cast();
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            std::ptr::null_mut(),
            system_sid,
            &mut system_len,
        )
    } == 0
    {
        return Err(internal(std::io::Error::last_os_error().to_string()));
    }
    let trustee = |sid: PSID| TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: 0,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_USER,
        ptstrName: sid.cast::<u16>(),
    };
    let entries = [
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: trustee(user_sid),
        },
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: trustee(system_sid),
        },
    ];
    let mut acl = std::ptr::null_mut();
    let status = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            std::ptr::null(),
            &mut acl,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    struct LocalGuard(*mut c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let _acl = LocalGuard(acl.cast());
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    verify_windows_private_dacl(path, user_sid, system_sid)
}

#[cfg(windows)]
fn verify_windows_private_dacl(
    path: &Path,
    user_sid: windows_sys::Win32::Security::PSID,
    system_sid: windows_sys::Win32::Security::PSID,
) -> DshApplicationResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_ALL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
        EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut acl = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(internal(
            std::io::Error::from_raw_os_error(status as i32).to_string(),
        ));
    }
    struct LocalGuard(*mut c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let _descriptor = LocalGuard(descriptor.cast());
    if acl.is_null() {
        return Err(DshApplicationError::precondition(
            "Windows private DACL is missing",
        ));
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(DshApplicationError::precondition(
            "Windows private DACL is not protected",
        ));
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    if unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
        || info.AceCount != 2
    {
        return Err(DshApplicationError::precondition(
            "Windows private DACL contains unexpected access entries",
        ));
    }
    let mut saw_user = false;
    let mut saw_system = false;
    for index in 0..info.AceCount {
        let mut raw_ace: *mut c_void = std::ptr::null_mut();
        if unsafe { GetAce(acl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            return Err(internal(std::io::Error::last_os_error().to_string()));
        }
        let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE
            || ace.Header.AceFlags != 0
            || (ace.Mask != GENERIC_ALL && ace.Mask != FILE_ALL_ACCESS)
        {
            return Err(DshApplicationError::precondition(
                "Windows private DACL contains an unexpected access rule",
            ));
        }
        let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
        if unsafe { EqualSid(sid, user_sid) } != 0 {
            if saw_user {
                return Err(DshApplicationError::precondition(
                    "Windows private DACL repeats the user rule",
                ));
            }
            saw_user = true;
        } else if unsafe { EqualSid(sid, system_sid) } != 0 {
            if saw_system {
                return Err(DshApplicationError::precondition(
                    "Windows private DACL repeats the system rule",
                ));
            }
            saw_system = true;
        } else {
            return Err(DshApplicationError::precondition(
                "Windows private DACL grants access to another identity",
            ));
        }
    }
    if !saw_user || !saw_system {
        return Err(DshApplicationError::precondition(
            "Windows private DACL does not protect the current user",
        ));
    }
    Ok(())
}
