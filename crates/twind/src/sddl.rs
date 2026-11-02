//! The access rules for the pipe and the data folder, as SDDL strings.
//!
//! Plain text, so the rules can be read and tested without touching the system.

/// Network logons are refused first, because a deny placed after an allow does nothing for
/// someone the allow already covers. `PIPE_REJECT_REMOTE_CLIENTS` is the other half of this.
const DENY_NETWORK: &str = "(D;;GA;;;NU)";

/// A SID is the only thing spliced into these strings. Refusing anything else keeps a bad
/// value from adding ACEs of its own.
fn checked(sid: &str) -> Result<&str, String> {
    let digits = sid.strip_prefix("S-").unwrap_or("");
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        Ok(sid)
    } else {
        Err(format!("{sid:?} isn't a SID"))
    }
}

/// `FILE_GENERIC_READ | FILE_GENERIC_WRITE` without `FILE_CREATE_PIPE_INSTANCE` (0x4).
///
/// Plain `GW` includes that right, and with it a reader could add a pipe end of its own and
/// answer other viewers. Windows doesn't require it to open a pipe as a client, even when
/// the client asks for `GENERIC_WRITE` as `ReadClient` does.
pub const READER_RIGHTS: &str = "0x12019B";

/// The read pipe under the service manager. Without the readers group only administrators
/// get in, which is what the service falls back to when the group has been deleted.
///
/// `OW` covers the process that makes the pipe, as long as it ends up the owner. Naming the
/// service's own account as well means its pipe instances don't depend on that.
pub fn service_pipe(service_sid: &str, readers_sid: Option<&str>) -> Result<String, String> {
    let readers = match readers_sid {
        Some(sid) => format!("(A;;{READER_RIGHTS};;;{})", checked(sid)?),
        None => String::new(),
    };
    Ok(format!("D:P{DENY_NETWORK}(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GA;;;{}){readers}", checked(service_sid)?))
}

/// The read pipe in console mode: nobody but the user who started it.
pub fn console_pipe(user_sid: &str) -> Result<String, String> {
    Ok(format!("D:P{DENY_NETWORK}(A;;GA;;;{})", checked(user_sid)?))
}

/// The data folder: protected from inheriting anything from `%ProgramData%`, whose defaults
/// let any user create files, and passed on to everything inside. Nobody else can even read
/// it, because the pipe is how anyone else sees this data.
pub fn data_dir(service_sid: &str) -> Result<String, String> {
    Ok(format!("O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})", checked(service_sid)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const SERVICE: &str = "S-1-5-80-1-2-3-4-5";

    fn aces(sddl: &str) -> Vec<&str> {
        sddl.split('(').skip(1).map(|ace| ace.trim_end_matches(')')).collect()
    }

    #[test]
    fn the_service_pipe_lets_readers_read_and_write_but_gives_them_nothing_more() {
        let sddl = service_pipe(SERVICE, Some(SID)).unwrap();
        assert_eq!(sddl, format!("D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GA;;;{SERVICE})(A;;0x12019B;;;{SID})"));
        const FILE_CREATE_PIPE_INSTANCE: u32 = 0x4;
        let rights = u32::from_str_radix(READER_RIGHTS.trim_start_matches("0x"), 16).unwrap();
        assert_eq!(rights & FILE_CREATE_PIPE_INSTANCE, 0);
    }

    #[test]
    fn without_the_group_the_service_pipe_is_for_administrators_only() {
        let sddl = service_pipe(SERVICE, None).unwrap();
        assert_eq!(sddl, format!("D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GA;;;{SERVICE})"));
    }

    #[test]
    fn network_logons_are_refused_before_anything_is_allowed() {
        for sddl in [service_pipe(SERVICE, Some(SID)).unwrap(), service_pipe(SERVICE, None).unwrap(), console_pipe(SID).unwrap()] {
            assert_eq!(aces(&sddl)[0], "D;;GA;;;NU", "{sddl}");
        }
    }

    #[test]
    fn the_console_pipe_allows_one_user() {
        let sddl = console_pipe(SID).unwrap();
        let allows: Vec<_> = aces(&sddl).into_iter().filter(|ace| ace.starts_with("A;")).collect();
        assert_eq!(allows, [format!("A;;GA;;;{SID}")]);
    }

    #[test]
    fn the_data_dir_stops_inheriting_and_is_owned_by_administrators() {
        let sddl = data_dir(SID).unwrap();
        assert!(sddl.starts_with("O:BAD:PAI"), "{sddl}");
        assert_eq!(aces(&sddl), ["A;OICI;FA;;;SY", "A;OICI;FA;;;BA", &format!("A;OICI;FA;;;{SID}")]);
    }

    #[test]
    fn anything_but_a_sid_is_refused() {
        for bad in ["", "S-", "SenseTwin Readers", "S-1-5)(A;;GA;;;WD", "BU"] {
            assert!(console_pipe(bad).is_err(), "{bad:?}");
            assert!(service_pipe(SERVICE, Some(bad)).is_err(), "{bad:?}");
            assert!(service_pipe(bad, Some(SID)).is_err(), "{bad:?}");
            assert!(data_dir(bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_accepts_every_string_we_build() {
        use crate::win::SecurityDescriptor;
        // Parsing only: this checks the text is valid without creating or changing anything.
        let sddls = [service_pipe(SERVICE, Some(SID)).unwrap(), service_pipe(SERVICE, None).unwrap(), console_pipe(SID).unwrap(), data_dir(SID).unwrap()];
        for sddl in sddls {
            SecurityDescriptor::from_sddl(&sddl).unwrap_or_else(|e| panic!("{sddl}: {e}"));
        }
    }
}
