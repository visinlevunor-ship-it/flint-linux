//! The authentication decision login makes.

use crate::files::{find_user, parse_passwd, shadow_hash, User};
use crate::hash::{is_locked, verify};

/// Hash of a random string nobody knows, verified for unknown or locked accounts so
/// that "no such user" takes as long as "wrong password".
const DUMMY_HASH: &str =
    "$flint1$100000$00112233445566778899aabbccddeeff$0000000000000000000000000000000000000000000000000000000000000000";

/// Check `name`/`password` against /etc/passwd and /etc/shadow text.
/// Returns the account on success. Locked accounts, unknown users, malformed
/// hashes and wrong passwords all give `None`.
pub fn authenticate(passwd: &str, shadow: &str, name: &str, password: &str) -> Option<User> {
    let users = parse_passwd(passwd);
    let user = find_user(&users, name);

    // Effective hash: the shadow entry, else a real hash stored directly in passwd.
    let stored = user.and_then(|u| {
        shadow_hash(shadow, &u.name).map(str::to_string).or_else(|| (u.passwd != "x").then(|| u.passwd.clone()))
    });

    match (user, stored) {
        (Some(u), Some(h)) if !is_locked(&h) => match verify(password, &h) {
            Ok(true) => Some(u.clone()),
            _ => None,
        },
        _ => {
            let _ = verify(password, DUMMY_HASH);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_password;

    fn fixture() -> (String, String) {
        let salt = [7u8; 16];
        let passwd = "root:x:0:0:root:/root:/bin/sh\nflint:x:1000:1000::/home/flint:/bin/sh\nlocked:x:1001:1001::/:/bin/sh\nnoshadow:x:1002:1002::/:/bin/sh\n".to_string();
        let shadow = format!(
            "root:{}:::::::\nflint:{}:::::::\nlocked:!{}:::::::\n",
            hash_password("rootpw", &salt, 10),
            hash_password("flintpw", &salt, 10),
            hash_password("lockedpw", &salt, 10),
        );
        (passwd, shadow)
    }

    #[test]
    fn right_password_logs_in() {
        let (p, s) = fixture();
        let u = authenticate(&p, &s, "flint", "flintpw").unwrap();
        assert_eq!((u.uid, u.gid), (1000, 1000));
        assert_eq!(authenticate(&p, &s, "root", "rootpw").unwrap().uid, 0);
    }

    #[test]
    fn everything_else_is_refused() {
        let (p, s) = fixture();
        assert!(authenticate(&p, &s, "flint", "wrong").is_none());
        assert!(authenticate(&p, &s, "flint", "").is_none());
        assert!(authenticate(&p, &s, "nobody", "flintpw").is_none());
        assert!(authenticate(&p, &s, "locked", "lockedpw").is_none(), "locked account");
        assert!(authenticate(&p, &s, "noshadow", "anything").is_none(), "no shadow entry, x in passwd");
        assert!(authenticate(&p, "", "flint", "flintpw").is_none());
    }

    #[test]
    fn hash_stored_in_passwd_is_accepted_without_shadow() {
        let h = hash_password("pw", &[1u8; 16], 10);
        let passwd = format!("a:{h}:5:5::/:/bin/sh\n");
        assert_eq!(authenticate(&passwd, "", "a", "pw").unwrap().uid, 5);
        assert!(authenticate(&passwd, "", "a", "nope").is_none());
    }
}
