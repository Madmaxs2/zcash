use core::mem::size_of_val;
use core::pin::Pin;

use incrementalmerkletree::{bridgetree, Altitude, Frontier, Hashable};
use orchard::{bundle::Authorized, tree::MerkleHashOrchard};
use tracing::error;
use zcash_primitives::{
    merkle_tree::{
        incremental::{read_frontier_v1, write_frontier_v1},
        CommitmentTree, HashSer,
    },
    transaction::components::Amount,
};

use crate::{streams::CppStream, wallet::Wallet};

pub const MERKLE_DEPTH: u8 = 32;

#[cxx::bridge]
mod ffi {
    extern "C++" {
        include!("streams.h");

        #[cxx_name = "RustDataStream"]
        type RustStream = crate::streams::ffi::RustStream;
    }

    #[namespace = "merkle_frontier"]
    extern "Rust" {
        type Orchard;
        type OrchardBundle;
        type OrchardWallet;

        fn orchard_empty_root() -> [u8; 32];
        fn new_orchard() -> Box<Orchard>;
        fn box_clone(self: &Orchard) -> Box<Orchard>;
        fn parse_orchard(stream: Pin<&mut RustStream>) -> Result<Box<Orchard>>;
        fn serialize(self: &Orchard, stream: Pin<&mut RustStream>) -> Result<()>;
        fn serialize_legacy(self: &Orchard, stream: Pin<&mut RustStream>) -> Result<()>;
        fn dynamic_memory_usage(self: &Orchard) -> usize;
        fn root(self: &Orchard) -> [u8; 32];
        fn size(self: &Orchard) -> u64;
        unsafe fn append_bundle(self: &mut Orchard, bundle: *const OrchardBundle) -> bool;
        unsafe fn init_wallet(self: &Orchard, wallet: *mut OrchardWallet) -> bool;
    }
}

type Inner<H> = bridgetree::Frontier<H, MERKLE_DEPTH>;

/// An incremental Merkle frontier.
#[derive(Clone)]
struct MerkleFrontier<H>(Inner<H>);

impl<H: Copy + Hashable + HashSer> MerkleFrontier<H> {
    /// Returns a copy of the value.
    fn box_clone(&self) -> Box<Self> {
        Box::new(self.clone())
    }

    /// Attempts to parse a Merkle frontier from the given C++ stream.
    fn parse(stream: Pin<&mut ffi::RustStream>) -> Result<Box<Self>, String> {
        let reader = CppStream::from(stream);

        match read_frontier_v1(reader) {
            Ok(parsed) => Ok(Box::new(MerkleFrontier(parsed))),
            Err(e) => Err(format!("Failed to parse v5 Merkle frontier: {}", e)),
        }
    }

    /// Serializes the frontier to the given C++ stream.
    fn serialize(&self, stream: Pin<&mut ffi::RustStream>) -> Result<(), String> {
        let writer = CppStream::from(stream);
        write_frontier_v1(writer, &self.0)
            .map_err(|e| format!("Failed to serialize v5 Merkle frontier: {}", e))
    }

    /// Serializes the frontier to the given C++ stream in the legacy frontier encoding.
    fn serialize_legacy(&self, stream: Pin<&mut ffi::RustStream>) -> Result<(), String> {
        let writer = CppStream::from(stream);
        let commitment_tree = CommitmentTree::from_frontier(&self.0);
        commitment_tree.write(writer).map_err(|e| {
            format!(
                "Failed to serialize Merkle frontier in legacy format: {}",
                e,
            )
        })
    }

    /// Returns the amount of memory dynamically allocated for the frontier.
    ///
    /// Includes `self` because this type is stored on the heap when passed to C++.
    fn dynamic_memory_usage(&self) -> usize {
        size_of_val(&self.0) + self.0.dynamic_memory_usage()
    }

    /// Obtains the current root of this Merkle frontier by hashing against empty nodes up
    /// to the maximum height of the pruned tree that the frontier represents.
    fn root(&self) -> [u8; 32] {
        let mut root = [0; 32];
        self.0
            .root()
            .write(&mut root[..])
            .expect("root is 32 bytes");
        root
    }

    /// Returns the number of leaves appended to the frontier.
    fn size(&self) -> u64 {
        self.0.position().map_or(0, |p| <u64>::from(p) + 1)
    }
}

/// Returns the root of an empty Orchard Merkle tree.
fn orchard_empty_root() -> [u8; 32] {
    let altitude = Altitude::from(MERKLE_DEPTH);
    MerkleHashOrchard::empty_root(altitude).to_bytes()
}

/// An Orchard incremental Merkle frontier.
type Orchard = MerkleFrontier<MerkleHashOrchard>;

/// Constructs a new empty Orchard Merkle frontier.
fn new_orchard() -> Box<Orchard> {
    Box::new(MerkleFrontier(Inner::empty()))
}

/// Attempts to parse an Orchard Merkle frontier from the given C++ stream.
fn parse_orchard(stream: Pin<&mut ffi::RustStream>) -> Result<Box<Orchard>, String> {
    Orchard::parse(stream)
}

struct OrchardBundle;
struct OrchardWallet;

impl Orchard {
    /// Appends the note commitments in the given bundle to this frontier.
    fn append_bundle(&mut self, bundle: *const OrchardBundle) -> bool {
        let bundle = unsafe { (bundle as *const orchard::Bundle<Authorized, Amount>).as_ref() };

        if let Some(bundle) = bundle {
            for action in bundle.actions().iter() {
                if !self.0.append(&MerkleHashOrchard::from_cmx(action.cmx())) {
                    error!("Orchard note commitment tree is full.");
                    return false;
                }
            }
        }

        true
    }

    /// Overwrites the first bridge of the Orchard wallet's note commitment tree to have
    /// `self` as its latest state.
    ///
    /// This will fail with an assertion error if any checkpoints exist in the tree.
    ///
    /// TODO: Remove once `crate::wallet` is migrated to `cxx`.
    fn init_wallet(&self, wallet: *mut OrchardWallet) -> bool {
        crate::wallet::orchard_wallet_init_from_frontier(wallet as *mut Wallet, &self.0)
    }
}