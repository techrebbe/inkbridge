mock_provider "google" {}

run "disabled_is_empty" {
  command = plan

  assert {
    condition     = output.deployment_stage == "disabled"
    error_message = "The default stage must remain disabled."
  }

  assert {
    condition     = length(google_project_service.required) == 0
    error_message = "Disabled mode must not plan project APIs."
  }

  assert {
    condition     = length(google_artifact_registry_repository.runtime) == 0
    error_message = "Disabled mode must not plan Artifact Registry."
  }
}

run "acknowledgement_guard_rejects_partial_opt_in" {
  command = plan

  variables {
    enable_deployment = true
  }

  expect_failures = [terraform_data.deployment_guard[0]]
}

run "bootstrap_omits_runtime" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
  }

  assert {
    condition     = output.deployment_stage == "bootstrap"
    error_message = "An empty image must select bootstrap."
  }

  assert {
    condition     = length(google_artifact_registry_repository.runtime) == 1
    error_message = "Bootstrap must create the image repository."
  }

  assert {
    condition     = length(google_service_account.builder) == 1
    error_message = "Bootstrap must create the least-privilege image builder."
  }

  assert {
    condition     = length(google_storage_bucket.build_source) == 1
    error_message = "Bootstrap must create a dedicated build-source bucket."
  }

  assert {
    condition = anytrue([
      for policy in google_artifact_registry_repository.runtime[0].cleanup_policies :
      policy.id == "delete-old-builds" && try(policy.condition[0].tag_state == "TAGGED", false)
    ])
    error_message = "Commit-tagged build images must be eligible for cleanup."
  }

  assert {
    condition = anytrue([
      for policy in google_artifact_registry_repository.runtime[0].cleanup_policies :
      policy.id == "keep-deployed" && try(contains(policy.condition[0].tag_prefixes, "deployed-"), false)
    ])
    error_message = "The deployed image tag must be explicitly protected from cleanup."
  }

  assert {
    condition = (
      google_storage_bucket_iam_member.builder_source[0].bucket ==
      google_storage_bucket.build_source[0].name
    )
    error_message = "The builder source-reader grant must be scoped to the dedicated source bucket."
  }

  assert {
    condition = (
      length(google_storage_bucket.sync[0].lifecycle_rule) == 2 &&
      alltrue([
        for rule in google_storage_bucket.sync[0].lifecycle_rule :
        contains(try(rule.condition[0].matches_prefix, []), "Staging/") &&
        !contains(try(rule.condition[0].matches_prefix, []), "BrokerOutbox/")
      ])
    )
    error_message = "The data bucket must expire staging objects without age-deleting recoverable outbox payloads."
  }

  assert {
    condition     = length(google_cloud_run_v2_service.runtime) == 0
    error_message = "Bootstrap must not create Cloud Run."
  }

  assert {
    condition     = length(google_eventarc_trigger.storage_finalized) == 0
    error_message = "Bootstrap must not create Eventarc."
  }
}

run "immutable_digest_enables_runtime" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    cloud_run_image                = "me-west1-docker.pkg.dev/inkbridge-plan-test/inkbridge/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }

  assert {
    condition     = output.deployment_stage == "runtime"
    error_message = "An immutable image digest must select runtime."
  }

  assert {
    condition     = length(google_cloud_run_v2_service.runtime) == 1
    error_message = "Runtime must create Cloud Run."
  }

  assert {
    condition     = length(google_eventarc_trigger.storage_finalized) == 1
    error_message = "Runtime must create Eventarc."
  }

  assert {
    condition = (
      google_cloud_run_v2_service.runtime[0].template[0].timeout == "900s" &&
      google_cloud_run_v2_service.runtime[0].template[0].max_instance_request_concurrency == 1 &&
      google_cloud_run_v2_service.runtime[0].template[0].containers[0].resources[0].limits["cpu"] == "2" &&
      google_cloud_run_v2_service.runtime[0].template[0].containers[0].resources[0].limits["memory"] == "8Gi"
    )
    error_message = "Runtime must use the reviewed single-request large-PDF resource envelope."
  }
}

run "foreign_image_digest_is_rejected" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    cloud_run_image                = "me-west1-docker.pkg.dev/another-project/inkbridge/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }

  expect_failures = [terraform_data.deployment_guard[0]]
}
