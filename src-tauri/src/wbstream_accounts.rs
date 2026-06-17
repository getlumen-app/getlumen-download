#[cfg(test)]
use std::collections::BTreeMap;

const RATE_LIMIT_COOLDOWN_MS: u64 = 60_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountStatus {
    Healthy,
    Degraded,
    RateLimited,
    CookieRefreshing,
    NeedsReauth,
    Disabled,
    RevokedByOwner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomLease {
    pub account_alias: String,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AllocationError {
    NoHealthyAccounts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    RateLimited,
    NeedsReauth,
    Disabled,
}

pub trait WbAccountProvider {
    fn account_aliases(&self) -> Vec<String>;
    fn account_status(&self, alias: &str) -> Option<AccountStatus>;
    fn refresh_session(&mut self, alias: &str) -> Result<(), ProviderError>;
    fn create_room(&mut self, alias: &str) -> Result<RoomLease, ProviderError>;
    fn mark_rate_limited(&mut self, alias: &str, cooldown_until_ms: u64);
    fn mark_needs_reauth(&mut self, alias: &str);
}

#[derive(Debug, Default)]
pub struct RoomAllocator {
    next_index: usize,
}

impl RoomAllocator {
    pub fn allocate_room<P: WbAccountProvider>(
        &mut self,
        provider: &mut P,
        now_ms: u64,
    ) -> Result<RoomLease, AllocationError> {
        let aliases = provider.account_aliases();
        if aliases.is_empty() {
            return Err(AllocationError::NoHealthyAccounts);
        }

        for offset in 0..aliases.len() {
            let index = (self.next_index + offset) % aliases.len();
            let alias = &aliases[index];
            if !self.prepare_account(provider, alias, now_ms) {
                continue;
            }

            match provider.create_room(alias) {
                Ok(room) => {
                    self.next_index = (index + 1) % aliases.len();
                    return Ok(room);
                }
                Err(ProviderError::RateLimited) => {
                    provider.mark_rate_limited(alias, now_ms + RATE_LIMIT_COOLDOWN_MS);
                }
                Err(ProviderError::NeedsReauth) => provider.mark_needs_reauth(alias),
                Err(ProviderError::Disabled) => {}
            }
        }

        Err(AllocationError::NoHealthyAccounts)
    }

    fn prepare_account<P: WbAccountProvider>(
        &self,
        provider: &mut P,
        alias: &str,
        _now_ms: u64,
    ) -> bool {
        match provider.account_status(alias) {
            Some(AccountStatus::Healthy | AccountStatus::Degraded) => true,
            Some(AccountStatus::CookieRefreshing) => match provider.refresh_session(alias) {
                Ok(()) => true,
                Err(ProviderError::NeedsReauth) => {
                    provider.mark_needs_reauth(alias);
                    false
                }
                Err(ProviderError::RateLimited) => false,
                Err(ProviderError::Disabled) => false,
            },
            Some(
                AccountStatus::RateLimited
                | AccountStatus::NeedsReauth
                | AccountStatus::Disabled
                | AccountStatus::RevokedByOwner,
            )
            | None => false,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct FakeAccount {
    alias: String,
    status: AccountStatus,
    refreshable: bool,
    create_room_error: Option<ProviderError>,
    room_count: u64,
    refresh_count: u64,
    cooldown_until_ms: Option<u64>,
}

#[cfg(test)]
impl FakeAccount {
    fn healthy(alias: &str) -> Self {
        Self {
            alias: alias.to_string(),
            status: AccountStatus::Healthy,
            refreshable: true,
            create_room_error: None,
            room_count: 0,
            refresh_count: 0,
            cooldown_until_ms: None,
        }
    }

    fn expired_refreshable(alias: &str) -> Self {
        Self {
            status: AccountStatus::CookieRefreshing,
            ..Self::healthy(alias)
        }
    }

    fn expired_needs_reauth(alias: &str) -> Self {
        Self {
            status: AccountStatus::CookieRefreshing,
            refreshable: false,
            ..Self::healthy(alias)
        }
    }

    fn rate_limited(alias: &str) -> Self {
        Self {
            create_room_error: Some(ProviderError::RateLimited),
            ..Self::healthy(alias)
        }
    }

    fn disabled(alias: &str) -> Self {
        Self {
            status: AccountStatus::Disabled,
            ..Self::healthy(alias)
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
struct FakeWbAccountProvider {
    accounts: BTreeMap<String, FakeAccount>,
}

#[cfg(test)]
impl FakeWbAccountProvider {
    fn new(accounts: Vec<FakeAccount>) -> Self {
        Self {
            accounts: accounts
                .into_iter()
                .map(|account| (account.alias.clone(), account))
                .collect(),
        }
    }

    fn account(&self, alias: &str) -> &FakeAccount {
        self.accounts.get(alias).expect("fake account exists")
    }
}

#[cfg(test)]
impl WbAccountProvider for FakeWbAccountProvider {
    fn account_aliases(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }

    fn account_status(&self, alias: &str) -> Option<AccountStatus> {
        self.accounts
            .get(alias)
            .map(|account| account.status.clone())
    }

    fn refresh_session(&mut self, alias: &str) -> Result<(), ProviderError> {
        let account = self
            .accounts
            .get_mut(alias)
            .ok_or(ProviderError::Disabled)?;
        account.refresh_count += 1;
        if account.refreshable {
            account.status = AccountStatus::Healthy;
            Ok(())
        } else {
            account.status = AccountStatus::NeedsReauth;
            Err(ProviderError::NeedsReauth)
        }
    }

    fn create_room(&mut self, alias: &str) -> Result<RoomLease, ProviderError> {
        let account = self
            .accounts
            .get_mut(alias)
            .ok_or(ProviderError::Disabled)?;
        if let Some(error) = account.create_room_error.clone() {
            return Err(error);
        }
        account.room_count += 1;
        Ok(RoomLease {
            account_alias: alias.to_string(),
            url: format!("wbstream://{}-room-{}", alias, account.room_count),
        })
    }

    fn mark_rate_limited(&mut self, alias: &str, cooldown_until_ms: u64) {
        if let Some(account) = self.accounts.get_mut(alias) {
            account.status = AccountStatus::RateLimited;
            account.cooldown_until_ms = Some(cooldown_until_ms);
        }
    }

    fn mark_needs_reauth(&mut self, alias: &str) {
        if let Some(account) = self.accounts.get_mut(alias) {
            account.status = AccountStatus::NeedsReauth;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_refreshes_expired_session_without_owner_when_provider_can_refresh() {
        let mut provider = FakeWbAccountProvider::new(vec![
            FakeAccount::expired_refreshable("wb-a1"),
            FakeAccount::healthy("wb-a2"),
        ]);
        let mut allocator = RoomAllocator::default();

        let room = allocator.allocate_room(&mut provider, 1_000).unwrap();

        assert_eq!(room.account_alias, "wb-a1");
        assert_eq!(room.url, "wbstream://wb-a1-room-1");
        assert_eq!(provider.account("wb-a1").status, AccountStatus::Healthy);
        assert_eq!(provider.account("wb-a1").refresh_count, 1);
    }

    #[test]
    fn allocator_excludes_account_when_refresh_requires_owner_reauth() {
        let mut provider = FakeWbAccountProvider::new(vec![
            FakeAccount::expired_needs_reauth("wb-a1"),
            FakeAccount::healthy("wb-a2"),
        ]);
        let mut allocator = RoomAllocator::default();

        let room = allocator.allocate_room(&mut provider, 1_000).unwrap();

        assert_eq!(room.account_alias, "wb-a2");
        assert_eq!(provider.account("wb-a1").status, AccountStatus::NeedsReauth);
        assert_eq!(provider.account("wb-a1").refresh_count, 1);
    }

    #[test]
    fn allocator_sets_cooldown_after_rate_limit_and_uses_next_account() {
        let mut provider = FakeWbAccountProvider::new(vec![
            FakeAccount::rate_limited("wb-a1"),
            FakeAccount::healthy("wb-a2"),
        ]);
        let mut allocator = RoomAllocator::default();

        let room = allocator.allocate_room(&mut provider, 1_000).unwrap();

        assert_eq!(room.account_alias, "wb-a2");
        assert_eq!(provider.account("wb-a1").status, AccountStatus::RateLimited);
        assert_eq!(provider.account("wb-a1").cooldown_until_ms, Some(61_000));
    }

    #[test]
    fn allocator_balances_rooms_across_healthy_accounts() {
        let mut provider = FakeWbAccountProvider::new(vec![
            FakeAccount::healthy("wb-a1"),
            FakeAccount::healthy("wb-a2"),
            FakeAccount::healthy("wb-a3"),
        ]);
        let mut allocator = RoomAllocator::default();

        let first = allocator.allocate_room(&mut provider, 1_000).unwrap();
        let second = allocator.allocate_room(&mut provider, 2_000).unwrap();
        let third = allocator.allocate_room(&mut provider, 3_000).unwrap();

        assert_eq!(first.account_alias, "wb-a1");
        assert_eq!(second.account_alias, "wb-a2");
        assert_eq!(third.account_alias, "wb-a3");
    }

    #[test]
    fn allocator_returns_needs_capacity_when_no_accounts_can_create_rooms() {
        let mut provider = FakeWbAccountProvider::new(vec![
            FakeAccount::expired_needs_reauth("wb-a1"),
            FakeAccount::disabled("wb-a2"),
        ]);
        let mut allocator = RoomAllocator::default();

        let err = allocator.allocate_room(&mut provider, 1_000).unwrap_err();

        assert_eq!(err, AllocationError::NoHealthyAccounts);
    }
}
