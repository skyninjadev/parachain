// Copyright 2021 Integritee AG and Supercomputing Systems AG
// This file is part of the "Integritee parachain" and is
// based on Cumulus from Parity Technologies (UK) Ltd.

// Integritee parachain is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Cumulus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Integritee parachain.  If not, see <http://www.gnu.org/licenses/>.

//! XCM configuration for Integritee Runtime.
//!

use super::{
	AccountId, Balance, Balances, Convert, MaxInstructions, ParachainInfo, ParachainSystem,
	PolkadotXcm, Runtime, RuntimeCall, RuntimeEvent, RuntimeOrigin, XcmpQueue, TEER,
};
use codec::{Decode, Encode, MaxEncodedLen};
use core::marker::PhantomData;
use cumulus_primitives_core::GlobalConsensus;
use frame_support::{
	pallet_prelude::{Get, Weight},
	parameter_types,
	traits::{Everything, Nothing},
	weights::IdentityFee,
	RuntimeDebug,
};
use frame_system::EnsureRoot;
use orml_traits::{
	location::{RelativeReserveProvider, Reserve},
	parameter_type_with_key,
};
use orml_xcm_support::{IsNativeConcrete, MultiNativeAsset};
use pallet_xcm::XcmPassthrough;
use parachains_common::xcm_config::{DenyReserveTransferToRelayChain, DenyThenTry};
use polkadot_parachain::primitives::Sibling;
use scale_info::TypeInfo;
use sp_core::ConstU32;
use sp_std::{
	convert::{From, Into},
	prelude::*,
};
use xcm::latest::prelude::*;
use xcm_builder::{
	AccountId32Aliases, AllowKnownQueryResponses, AllowSubscriptionsFrom,
	AllowTopLevelPaidExecutionFrom, CurrencyAdapter, EnsureXcmOrigin, FixedWeightBounds,
	ParentAsSuperuser, ParentIsPreset, RelayChainAsNative, SiblingParachainAsNative,
	SiblingParachainConvertsVia, SignedAccountId32AsNative, SignedToAccountId32,
	SovereignSignedViaLocation, TakeWeightCredit, UsingComponents,
};
use xcm_executor::XcmExecutor;
use xcm_transactor_primitives::*;

const fn teer_general_key() -> Junction {
	const TEER_KEY: [u8; 32] = *b"TEER0000000000000000000000000000";
	GeneralKey { length: 4, data: TEER_KEY }
}

const TEER_GENERAL_KEY: Junction = teer_general_key();

parameter_types! {
	pub const RelayChainLocation: MultiLocation = MultiLocation::parent();
	pub const RelayNetwork: NetworkId = NetworkId::Kusama;
	pub RelayChainOrigin: RuntimeOrigin = cumulus_pallet_xcm::Origin::Relay.into();
	// The universal location within the global consensus system
	pub UniversalLocation: InteriorMultiLocation =
		X2(GlobalConsensus(RelayNetwork::get()), Parachain(ParachainInfo::parachain_id().into()));

	pub SelfReserve: MultiLocation = MultiLocation {
		parents:0,
		interior: Junctions::X1(TEER_GENERAL_KEY)
	};
}

// Supported Currencies.
#[derive(
	Encode,
	Decode,
	Eq,
	PartialEq,
	Copy,
	Clone,
	RuntimeDebug,
	PartialOrd,
	Ord,
	TypeInfo,
	MaxEncodedLen,
)]
pub enum CurrencyId {
	TEER,
}

/// Converts a CurrencyId into a Multilocation, used by xtoken for XCMP.
pub struct CurrencyIdConvert;
impl Convert<CurrencyId, Option<MultiLocation>> for CurrencyIdConvert {
	fn convert(id: CurrencyId) -> Option<MultiLocation> {
		match id {
			CurrencyId::TEER => Some(MultiLocation::new(
				1,
				X2(Parachain(ParachainInfo::parachain_id().into()), TEER_GENERAL_KEY),
			)),
		}
	}
}

/// Converts a Mulitloaction into a CurrencyId. Used by XCMP LocalAssetTransactor for asset filtering:
/// we only accept Assets that are convertable to a "CurrencyId".
impl Convert<MultiLocation, Option<CurrencyId>> for CurrencyIdConvert {
	fn convert(location: MultiLocation) -> Option<CurrencyId> {
		let self_para_id: u32 = ParachainInfo::parachain_id().into();

		match location {
			MultiLocation { parents, interior } if parents == 1 => match interior {
				X2(Parachain(para_id), junction)
					if junction == TEER_GENERAL_KEY && para_id == self_para_id =>
					Some(CurrencyId::TEER),
				_ => None,
			},
			MultiLocation { parents, interior } if parents == 0 => match interior {
				X1(junction) if junction == TEER_GENERAL_KEY => Some(CurrencyId::TEER),
				_ => None,
			},
			_ => None,
		}
	}
}

/// Converts a MultiAsset into a CurrencyId, using the defined Mulitlocation.
impl Convert<MultiAsset, Option<CurrencyId>> for CurrencyIdConvert {
	fn convert(asset: MultiAsset) -> Option<CurrencyId> {
		if let MultiAsset { id: Concrete(location), .. } = asset {
			Self::convert(location)
		} else {
			None
		}
	}
}

/// The means for routing XCM messages which are not for local execution into the right message
/// queues.
pub type XcmRouter = (
	// Two routers - use UMP to communicate with the relay chain:
	cumulus_primitives_utility::ParentAsUmp<ParachainSystem, PolkadotXcm, ()>,
	// ..and XCMP to communicate with the sibling chains.
	XcmpQueue,
);

/// Type for specifying how a `MultiLocation` can be converted into an `AccountId`. This is used
/// when determining ownership of accounts for asset transacting and when attempting to use XCM
/// `Transact` in order to determine the dispatch Origin.
pub type LocationToAccountId = (
	// The parent (Relay-chain) origin converts to the default `AccountId`.
	ParentIsPreset<AccountId>,
	// Sibling parachain origins convert to AccountId via the `ParaId::into`.
	SiblingParachainConvertsVia<Sibling, AccountId>,
	// Straight up local `AccountId32` origins just alias directly to `AccountId`.
	AccountId32Aliases<RelayNetwork, AccountId>,
);

/// Means for transacting assets on this chain.
pub type LocalAssetTransactor = CurrencyAdapter<
	// Use this currency:
	Balances,
	// Matcher: matches concrete fungible assets whose `id` could be converted into `CurrencyId`.
	IsNativeConcrete<CurrencyId, CurrencyIdConvert>,
	// Do a simple punn to convert an AccountId32 MultiLocation into a native chain account ID:
	LocationToAccountId,
	// Our chain's account ID type (we can't get away without mentioning it explicitly):
	AccountId,
	// We don't track any teleports.
	(),
>;

/// This is the type we use to convert an (incoming) XCM origin into a local `Origin` instance,
/// ready for dispatching a transaction with Xcm's `Transact`. There is an `OriginKind` which can
/// biases the kind of local `Origin` it will become.
pub type XcmOriginToTransactDispatchOrigin = (
	// Sovereign account converter; this attempts to derive an `AccountId` from the origin location
	// using `LocationToAccountId` and then turn that into the usual `Signed` origin. Useful for
	// foreign chains who want to have a local sovereign account on this chain which they control.
	SovereignSignedViaLocation<LocationToAccountId, RuntimeOrigin>,
	// Native converter for Relay-chain (Parent) location; will converts to a `Relay` origin when
	// recognised.
	RelayChainAsNative<RelayChainOrigin, RuntimeOrigin>,
	// Native converter for sibling Parachains; will convert to a `SiblingPara` origin when
	// recognised.
	SiblingParachainAsNative<cumulus_pallet_xcm::Origin, RuntimeOrigin>,
	// Superuser converter for the Relay-chain (Parent) location. This will allow it to issue a
	// transaction from the Root origin.
	ParentAsSuperuser<RuntimeOrigin>,
	// Native signed account converter; this just converts an `AccountId32` origin into a normal
	// `Origin::Signed` origin of the same 32-byte value.
	SignedAccountId32AsNative<RelayNetwork, RuntimeOrigin>,
	// Xcm origins can be represented natively under the Xcm pallet's Xcm origin.
	XcmPassthrough<RuntimeOrigin>,
);

/// This struct offers uses RelativeReserveProvider to output relative views of multilocations
/// However, additionally accepts a MultiLocation that aims at representing the chain part
/// (parent: 1, Parachain(paraId)) of the absolute representation of our chain.
/// If a token reserve matches against this absolute view, we return  Some(MultiLocation::here())
/// This helps users by preventing errors when they try to transfer a token through xtokens
/// to our chain (either inserting the relative or the absolute value).
pub struct AbsoluteAndRelativeReserve<AbsoluteMultiLocation>(PhantomData<AbsoluteMultiLocation>);
impl<AbsoluteMultiLocation> Reserve for AbsoluteAndRelativeReserve<AbsoluteMultiLocation>
where
	AbsoluteMultiLocation: Get<MultiLocation>,
{
	fn reserve(asset: &MultiAsset) -> Option<MultiLocation> {
		RelativeReserveProvider::reserve(asset).map(|relative_reserve| {
			if relative_reserve == AbsoluteMultiLocation::get() {
				MultiLocation::here()
			} else {
				relative_reserve
			}
		})
	}
}

parameter_types! {
	// Weight for one XCM operation. Copied from moonbeam.
	pub UnitWeightCost: Weight = Weight::from_parts(200_000_000u64, DEFAULT_PROOF_SIZE);
	// One TEER buys 1 second of weight.
	pub const WeightPrice: (MultiLocation, u128) = (MultiLocation::parent(), TEER);
}

pub type Barrier = DenyThenTry<
	DenyReserveTransferToRelayChain,
	(
		TakeWeightCredit,
		AllowTopLevelPaidExecutionFrom<Everything>,
		// Expected responses are OK.
		AllowKnownQueryResponses<PolkadotXcm>,
		// Subscriptions for version tracking are OK.
		AllowSubscriptionsFrom<Everything>,
	),
>;

pub struct SafeCallFilter;
impl frame_support::traits::Contains<RuntimeCall> for SafeCallFilter {
	fn contains(_call: &RuntimeCall) -> bool {
		// This is safe, as we prevent arbitrary xcm-transact executions.
		// For rationale, see:https://github.com/paritytech/polkadot/blob/19fdd197aff085f7f66e54942999fd536e7df475/runtime/kusama/src/xcm_config.rs#L171
		true
	}
}

parameter_types! {
	pub const MaxAssetsIntoHolding: u32 = 64;
}

pub struct XcmExecutorConfig;
impl xcm_executor::Config for XcmExecutorConfig {
	type RuntimeCall = RuntimeCall;
	type XcmSender = XcmRouter;
	// How to withdraw and deposit an asset.
	type AssetTransactor = LocalAssetTransactor;
	type OriginConverter = XcmOriginToTransactDispatchOrigin;
	type IsReserve = MultiNativeAsset<AbsoluteAndRelativeReserve<SelfLocationAbsolute>>;
	type IsTeleporter = (); // No teleport for now. Better be safe than sorry.
	type UniversalLocation = UniversalLocation;
	type Barrier = Barrier;
	type Weigher = FixedWeightBounds<UnitWeightCost, RuntimeCall, MaxInstructions>;
	type Trader = UsingComponents<IdentityFee<Balance>, SelfReserve, AccountId, Balances, ()>;
	type ResponseHandler = PolkadotXcm;
	type SubscriptionService = PolkadotXcm;
	type AssetTrap = PolkadotXcm;
	type AssetClaims = PolkadotXcm;
	type CallDispatcher = RuntimeCall;
	type PalletInstancesInfo = crate::AllPalletsWithSystem;
	type MaxAssetsIntoHolding = MaxAssetsIntoHolding;
	type AssetLocker = ();
	type AssetExchanger = ();
	type FeeManager = ();
	type MessageExporter = ();
	type UniversalAliases = Nothing;
	type SafeCallFilter = SafeCallFilter;
}

#[cfg(feature = "runtime-benchmarks")]
parameter_types! {
	pub ReachableDest: Option<MultiLocation> = Some(Parent.into());
}

// Converts a Signed Local Origin into a MultiLocation
pub type LocalOriginToLocation = SignedToAccountId32<RuntimeOrigin, AccountId, RelayNetwork>;

// FIXME: We should probably update the configuration here.
// See acala and moonbeam example : https://github.com/integritee-network/parachain/issues/103
impl pallet_xcm::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type SendXcmOrigin = EnsureXcmOrigin<RuntimeOrigin, ()>; // Prohibit sending arbitrary XCMs from users of this chain
	type XcmRouter = XcmRouter;
	type ExecuteXcmOrigin = EnsureXcmOrigin<RuntimeOrigin, LocalOriginToLocation>; // Allow any local origin in XCM execution.
	type XcmExecuteFilter = Nothing; // Disable generic XCM execution. This does not affect Teleport or Reserve Transfer.
	type XcmExecutor = XcmExecutor<XcmExecutorConfig>;
	type XcmTeleportFilter = Nothing; // Do not allow teleports
	type XcmReserveTransferFilter = Everything; // Transfer are allowed
	type Weigher = FixedWeightBounds<UnitWeightCost, RuntimeCall, MaxInstructions>;
	type UniversalLocation = UniversalLocation;
	type RuntimeOrigin = RuntimeOrigin;
	type RuntimeCall = RuntimeCall;
	const VERSION_DISCOVERY_QUEUE_SIZE: u32 = 100;
	type AdvertisedXcmVersion = pallet_xcm::CurrentXcmVersion;
	type Currency = Balances;
	type CurrencyMatcher = ();
	type TrustedLockers = ();
	type SovereignAccountOf = LocationToAccountId;
	type MaxLockers = ConstU32<8>;
	// TODO pallet-xcm weights
	type WeightInfo = pallet_xcm::TestWeightInfo;
	#[cfg(feature = "runtime-benchmarks")]
	type ReachableDest = ReachableDest;
	type AdminOrigin = EnsureRoot<AccountId>;
}

parameter_types! {
	pub const IntegriteeKsmParaId: u32 = 2015;
	pub const ShellRuntimeParaId: u32 = 2267;
}

impl pallet_xcm_transactor::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type RelayCallBuilder = RelayCallBuilder<ShellRuntimeParaId>;
	type XcmSender = XcmRouter;
	type SwapOrigin = EnsureRoot<AccountId>;
	type ShellRuntimeParaId = ShellRuntimeParaId;
	type IntegriteeKsmParaId = IntegriteeKsmParaId;
	type WeightInfo = ();
}

impl cumulus_pallet_xcm::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type XcmExecutor = XcmExecutor<XcmExecutorConfig>;
}

// FIXME: Update to PolkadotXcm.
impl cumulus_pallet_xcmp_queue::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type XcmExecutor = XcmExecutor<XcmExecutorConfig>;
	type ChannelInfo = ParachainSystem;
	type VersionWrapper = ();
	type ExecuteOverweightOrigin = EnsureRoot<AccountId>;
	type ControllerOrigin = EnsureRoot<AccountId>;
	type ControllerOriginConverter = XcmOriginToTransactDispatchOrigin;
	type WeightInfo = cumulus_pallet_xcmp_queue::weights::SubstrateWeight<Runtime>;
	type PriceForSiblingDelivery = ();
}

impl cumulus_pallet_dmp_queue::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type XcmExecutor = XcmExecutor<XcmExecutorConfig>;
	type ExecuteOverweightOrigin = EnsureRoot<AccountId>;
}

parameter_types! {
	// This is how we are going to detect whether the asset is a Reserve asset
	pub SelfLocation: MultiLocation = MultiLocation::here();
	// We need this to be able to catch when someone is trying to execute a non-
	// cross-chain transfer in xtokens through the absolute path way
	pub SelfLocationAbsolute: MultiLocation = MultiLocation {
		parents:1,
		interior: Junctions::X1(
			Parachain(ParachainInfo::parachain_id().into())
		)
	};
}

/// Copied from moonbeam: https://github.com/PureStake/moonbeam/blob/095031d171b0c163e5649ee35acbc36eef681a82/primitives/xcm/src/ethereum_xcm.rs#L34
pub const DEFAULT_PROOF_SIZE: u64 = 128 * 1024;

parameter_types! {
	pub const BaseXcmWeight: Weight= Weight::from_parts(200_000_000u64, DEFAULT_PROOF_SIZE);
	pub const MaxAssetsForTransfer: usize = 2;
}

// The min fee amount in fee asset is split into two parts:
//
// - fee asset sent to fee reserve chain = fee_amount - min_xcm_fee
// - fee asset sent to dest reserve chain = min_xcm_fee
// Check out for more information:
// https://github.com/open-web3-stack/open-runtime-module-library/tree/master/xtokens#transfer-multiple-currencies

parameter_type_with_key! {
	pub ParachainMinFee: |_location: MultiLocation| -> Option<u128> {
		None
	};
}

pub struct AccountIdToMultiLocation;
impl Convert<AccountId, MultiLocation> for AccountIdToMultiLocation {
	fn convert(account: AccountId) -> MultiLocation {
		X1(AccountId32 { network: None, id: account.into() }).into()
	}
}
