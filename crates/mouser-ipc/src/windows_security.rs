//! Windows named-pipe security for the daemon IPC endpoint.

#![allow(unsafe_code)]

use std::ffi::{c_void, OsStr};
use std::io;
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FALSE, HANDLE,
    INVALID_HANDLE_VALUE, TRUE,
};
use windows_sys::Win32::Security::Authorization::{
    SetEntriesInAclW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, TRUSTEE_IS_SID,
    TRUSTEE_IS_USER,
};
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, InitializeSecurityDescriptor,
    SetSecurityDescriptorDacl, TokenUser, ACL, NO_INHERITANCE, PSID, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const PIPE_CLIENT_ACCESS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE;

#[derive(Clone)]
pub(super) struct UserSid {
    bytes: Vec<u8>,
}

impl UserSid {
    fn copy_from(sid: PSID) -> io::Result<Self> {
        if sid.is_null() {
            return Err(invalid_data("token user SID is null"));
        }
        let len = unsafe { GetLengthSid(sid) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = vec![0; len as usize];
        let dest = bytes.as_mut_ptr().cast::<c_void>();
        if unsafe { CopySid(len, dest, sid) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { bytes })
    }

    fn as_psid(&self) -> PSID {
        self.bytes.as_ptr().cast::<c_void>().cast_mut()
    }

    fn equals(&self, other: &Self) -> bool {
        unsafe { EqualSid(self.as_psid(), other.as_psid()) != FALSE }
    }
}

pub(super) fn current_process_user_sid() -> io::Result<UserSid> {
    let token = open_process_token(unsafe { GetCurrentProcess() })?;
    token_user_sid(token.raw())
}

pub(super) fn create_user_pipe(
    options: &ServerOptions,
    pipe_name: &OsStr,
    allowed_user: &UserSid,
) -> io::Result<NamedPipeServer> {
    let mut security = PipeSecurityAttributes::for_user(allowed_user)?;
    unsafe { options.create_with_security_attributes_raw(pipe_name, security.as_mut_ptr()) }
}

pub(super) fn verify_pipe_client_user(
    pipe: &NamedPipeServer,
    daemon_user: &UserSid,
) -> io::Result<()> {
    let client_pid = named_pipe_client_pid(pipe)?;
    let process = Handle::open_process(client_pid)?;
    let token = open_process_token(process.raw())?;
    let client_user = token_user_sid(token.raw())?;
    if client_user.equals(daemon_user) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "ipc pipe client user sid does not match daemon user sid",
    ))
}

struct PipeSecurityAttributes {
    _dacl: LocalAcl,
    _allowed_user: UserSid,
    _descriptor: Box<SECURITY_DESCRIPTOR>,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurityAttributes {
    fn for_user(user: &UserSid) -> io::Result<Self> {
        let dacl = LocalAcl::for_user(user)?;
        let mut descriptor = Box::<SECURITY_DESCRIPTOR>::default();
        let descriptor_ptr = descriptor.as_mut() as *mut SECURITY_DESCRIPTOR;
        if unsafe {
            InitializeSecurityDescriptor(
                descriptor_ptr.cast::<c_void>(),
                SECURITY_DESCRIPTOR_REVISION,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorDacl(descriptor_ptr.cast::<c_void>(), TRUE, dacl.as_ptr(), FALSE)
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_ptr.cast::<c_void>(),
            bInheritHandle: FALSE,
        };
        Ok(Self {
            _dacl: dacl,
            _allowed_user: user.clone(),
            _descriptor: descriptor,
            attributes,
        })
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        (&mut self.attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>()
    }
}

struct LocalAcl {
    ptr: *mut ACL,
}

impl LocalAcl {
    fn for_user(user: &UserSid) -> io::Result<Self> {
        let mut access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: PIPE_CLIENT_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: windows_sys::Win32::Security::Authorization::TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user.as_psid().cast::<u16>(),
            },
        };
        let mut acl = null_mut();
        let status = unsafe { SetEntriesInAclW(1, &mut access, null_mut(), &mut acl) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if acl.is_null() {
            return Err(invalid_data("SetEntriesInAclW returned a null DACL"));
        }
        Ok(Self { ptr: acl })
    }

    fn as_ptr(&self) -> *mut ACL {
        self.ptr
    }
}

impl Drop for LocalAcl {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let _ = LocalFree(self.ptr.cast::<c_void>());
            }
        }
    }
}

struct Handle(HANDLE);

impl Handle {
    fn open_process(pid: u32) -> io::Result<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn named_pipe_client_pid(pipe: &NamedPipeServer) -> io::Result<u32> {
    let mut pid = 0;
    let handle = pipe.as_raw_handle().cast::<c_void>();
    if unsafe { GetNamedPipeClientProcessId(handle, &mut pid) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

fn open_process_token(process: HANDLE) -> io::Result<Handle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    if token.is_null() {
        return Err(invalid_data("OpenProcessToken returned a null token"));
    }
    Ok(Handle(token))
}

fn token_user_sid(token: HANDLE) -> io::Result<UserSid> {
    let mut len = 0;
    if unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len) } != FALSE {
        return Err(invalid_data(
            "GetTokenInformation returned no token-user size",
        ));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || len == 0 {
        return Err(err);
    }

    let mut buffer = vec![0; len as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            len,
            &mut len,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    let token_user = buffer.as_ptr().cast::<TOKEN_USER>();
    let sid = unsafe { (*token_user).User.Sid };
    UserSid::copy_from(sid)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
