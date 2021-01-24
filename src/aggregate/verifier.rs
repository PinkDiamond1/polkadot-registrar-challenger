use crate::event::{Event, ExternalMessage, IdentityVerification};
use crate::state::{
    IdentityAddress, IdentityField, IdentityState, VerificationOutcome, VerificationStatus,
};
use crate::Result;
use eventually::Aggregate;
use futures::future::BoxFuture;
use std::marker::PhantomData;

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
#[serde(rename = "aggregate_verifier_id")]
pub struct VerifierAggregateId;

pub enum VerifierCommand {
    VerifyMessage(Event<ExternalMessage>),
    RequestState(IdentityAddress),
}

pub struct VerifierAggregate<'a> {
    _p: PhantomData<&'a ()>,
}

impl<'a> VerifierAggregate<'a> {
    fn handle_verify_message(
        state: &IdentityState<'a>,
        event: Event<ExternalMessage>,
    ) -> Result<Option<Vec<Event<IdentityVerification>>>> {
        let body = event.body();
        let (identity_field, provided_message) = (
            IdentityField::from((body.origin, body.field_address)),
            body.message,
        );

        // Verify message by acquiring a reader to the state.
        let verification_outcomes = state.verify_message(&identity_field, &provided_message);

        // If corresponding identities have been found, generate the
        // corresponding events.
        let mut events = vec![];
        for outcome in verification_outcomes {
            let net_address = outcome.net_address;

            events.push(match outcome.status {
                VerificationStatus::Valid => IdentityVerification {
                    net_address: net_address.clone(),
                    field: identity_field.clone(),
                    expected_message: outcome.expected_message.clone(),
                    is_valid: true,
                }
                .into(),
                VerificationStatus::Invalid => IdentityVerification {
                    net_address: net_address.clone(),
                    field: identity_field.clone(),
                    expected_message: outcome.expected_message.clone(),
                    is_valid: false,
                }
                .into(),
            });
        }

        if events.is_empty() {
            Ok(None)
        } else {
            Ok(Some(events))
        }
    }
    fn handle_state_request(
        state: &IdentityState<'a>,
        net_address: IdentityAddress,
    ) -> Result<Option<Vec<Event<IdentityVerification>>>> {
        Ok(None)
    }
    fn apply_state_changes(state: &mut IdentityState<'a>, event: Event<IdentityVerification>) {
        let body = event.body();
        let net_address = body.net_address;
        let field = body.field;

        if body.is_valid {
            // TODO: Handle `false`?
            state.set_verified(&net_address, &field);
        }
    }
}

impl<'is> Aggregate for VerifierAggregate<'is> {
    type Id = VerifierAggregateId;
    type State = IdentityState<'is>;
    type Event = Event<IdentityVerification>;
    // This aggregate has a single purpose. No commands required.
    type Command = VerifierCommand;
    type Error = failure::Error;

    fn apply(mut state: Self::State, event: Self::Event) -> Result<Self::State> {
        Self::apply_state_changes(&mut state, event);
        Ok(state)
    }

    fn handle<'a, 's>(
        &'a self,
        id: &'s Self::Id,
        state: &'s Self::State,
        command: Self::Command,
    ) -> BoxFuture<Result<Option<Vec<Self::Event>>>>
    where
        's: 'a,
    {
        let fut = async move {
            match command {
                VerifierCommand::VerifyMessage(event) => Self::handle_verify_message(state, event),
                VerifierCommand::RequestState(address) => {
                    Self::handle_state_request(state, address)
                }
            }
        };

        Box::pin(fut)
    }
}
