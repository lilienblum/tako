package tako

import (
	"tako.sh/internal"
)

type ChannelTransport = internal.ChannelTransport
type ChannelOperation = internal.ChannelOperation
type ChannelLifecycleConfig = internal.ChannelLifecycleConfig
type ChannelAuthContext = internal.ChannelAuthContext
type ChannelGrant = internal.ChannelGrant
type ChannelAuthDecision = internal.ChannelAuthDecision
type ChannelDefinition = internal.ChannelDefinition
type ChannelAuthRequest = internal.ChannelAuthRequest
type ChannelAuthorizeInput = internal.ChannelAuthorizeInput
type ChannelAuthorizeResponse = internal.ChannelAuthorizeResponse
type Channel = internal.Channel
type ChannelRegistry = internal.ChannelRegistry

const (
	ChannelTransportWS = internal.ChannelTransportWS

	ChannelOperationSubscribe = internal.ChannelOperationSubscribe
	ChannelOperationPublish   = internal.ChannelOperationPublish
	ChannelOperationConnect   = internal.ChannelOperationConnect
)

var Channels = internal.Channels

func AllowChannel(grant ChannelGrant) ChannelAuthDecision {
	return internal.AllowChannel(grant)
}

func RejectChannel() ChannelAuthDecision {
	return internal.RejectChannel()
}
