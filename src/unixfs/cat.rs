use crate::{Error, Ipfs, IpfsTypes};
use async_stream::stream;
use futures::stream::Stream;
use ipfs_unixfs::file::{visit::IdleFileVisit, FileReadFailed};
use libipld::cid::Cid;
use std::borrow::Borrow;
use std::fmt;
use std::ops::Range;

/// IPFS cat operation, producing a stream of file bytes. This is generic over the different kinds
/// of ways to own an `Ipfs` value in order to support both operating with borrowed `Ipfs` value
/// and an owned value. Passing an owned value allows the return value to be `'static`, which can
/// be helpful in some contexts, like the http.
///
/// Returns a stream of bytes on the file pointed with the Cid.
pub fn cat<'a, Types, MaybeOwned>(
    ipfs: MaybeOwned,
    cid: Cid,
    range: Option<Range<u64>>,
) -> impl Stream<Item = Result<Vec<u8>, TraversalFailed>> + Send + 'a
where
    Types: IpfsTypes,
    MaybeOwned: Borrow<Ipfs<Types>> + Send + 'a,
{
    use bitswap::Block;

    // using async_stream here at least to get on faster; writing custom streams is not too easy
    // but this might be easy enough to write open.
    stream! {
        let mut visit = IdleFileVisit::default();
        if let Some(range) = range {
            visit = visit.with_target_range(range);
        }

        // Get the root block to start the traversal. The stream does not expose any of the file
        // metadata. To get to it the user needs to create a Visitor over the first block.
        let borrow = ipfs.borrow();
        let Block { cid, data } = match borrow.get_block(&cid).await {
            Ok(block) => block,
            Err(e) => {
                yield Err(TraversalFailed::Loading(cid, e));
                return;
            }
        };

        // Start the visit from the root block.
        let mut visit = match visit.start(&data) {
            Ok((bytes, _, visit)) => {
                if !bytes.is_empty() {
                    yield Ok(bytes.to_vec());
                }

                match visit {
                    Some(v) => v,
                    None => return,
                }
            },
            Err(e) => {
                yield Err(TraversalFailed::Walking(cid, e));
                return;
            }
        };

        loop {
            // TODO: if it was possible, it would make sense to start downloading N of these
            let (next, _) = visit.pending_links();

            let borrow = ipfs.borrow();
            let Block { cid, data } = match borrow.get_block(&next).await {
                Ok(block) => block,
                Err(e) => {
                    yield Err(TraversalFailed::Loading(next.to_owned(), e));
                    return;
                },
            };

            match visit.continue_walk(&data) {
                Ok((bytes, next_visit)) => {
                    if !bytes.is_empty() {
                        // TODO: manual implementation could allow returning just the slice
                        yield Ok(bytes.to_vec());
                    }

                    match next_visit {
                        Some(v) => visit = v,
                        None => return,
                    }
                }
                Err(e) => {
                    yield Err(TraversalFailed::Walking(cid, e));
                    return;
                }
            }
        }
    }
}

/// Types of failures which can occur while walking the UnixFS graph.
#[derive(Debug)]
pub enum TraversalFailed {
    /// Failure to load the block
    Loading(Cid, Error),
    /// Processing of the block failed
    Walking(Cid, FileReadFailed),
}

impl fmt::Display for TraversalFailed {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TraversalFailed::*;
        match self {
            Loading(cid, e) => write!(fmt, "loading of {} failed: {}", cid, e),
            Walking(cid, e) => write!(fmt, "failed to walk {}: {}", cid, e),
        }
    }
}

impl std::error::Error for TraversalFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use TraversalFailed::*;

        match self {
            Loading(_, _) => {
                // FIXME: anyhow::Error cannot be given out as source.
                None
            }
            Walking(_, e) => Some(e),
        }
    }
}
