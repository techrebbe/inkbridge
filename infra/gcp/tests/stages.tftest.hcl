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

  assert {
    condition = (
      length(google_secret_manager_secret.drive_oauth_client) == 0 &&
      length(google_secret_manager_secret.drive_refresh_token) == 0 &&
      length(google_service_account.drive_runtime) == 0 &&
      length(google_cloud_run_v2_job.drive_runtime) == 0
    )
    error_message = "Disabled mode must not plan any Drive runtime resources."
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
    condition = (
      length(google_service_account.folder_transport) == 1 &&
      length(google_storage_bucket_iam_member.folder_transport_reader) == 1 &&
      length(google_storage_bucket_iam_member.folder_transport_device_writer) == 1
    )
    error_message = "Bootstrap must create the dedicated folder-transport identity and bucket grants."
  }

  assert {
    condition = (
      google_storage_bucket_iam_member.folder_transport_device_writer[0].role == "roles/storage.objectCreator" &&
      strcontains(one(google_storage_bucket_iam_member.folder_transport_device_writer[0].condition).expression, "/objects/BOOX_Folder/") &&
      strcontains(one(google_storage_bucket_iam_member.folder_transport_device_writer[0].condition).expression, "/objects/Supernote_Folder/") &&
      !strcontains(one(google_storage_bucket_iam_member.folder_transport_device_writer[0].condition).expression, "/objects/Conflicts/")
    )
    error_message = "The folder transport may create only device-folder objects, never conflict markers."
  }

  assert {
    condition     = length(google_service_account_iam_member.folder_transport_impersonator) == 0
    error_message = "No operator may impersonate the folder transport unless explicitly configured."
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
        contains(one(rule.condition).matches_prefix, "Staging/") &&
        !contains(one(rule.condition).matches_prefix, "BrokerOutbox/")
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

  assert {
    condition = (
      output.drive_runtime_stage == "bootstrap" &&
      length(google_secret_manager_secret.drive_oauth_client) == 1 &&
      length(google_secret_manager_secret.drive_refresh_token) == 1 &&
      length(google_service_account.drive_runtime) == 1 &&
      length(google_cloud_run_v2_job.drive_runtime) == 0
    )
    error_message = "Bootstrap must create empty Drive secret containers and its identity, but no job."
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
    folder_transport_operator      = "user:operator@example.com"
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
    condition     = google_cloud_run_v2_service.runtime[0].ingress == "INGRESS_TRAFFIC_ALL"
    error_message = "Runtime must be network-reachable so the IAM-authorized operator can use the conflict API."
  }

  assert {
    condition = (
      length(google_cloud_run_v2_service_iam_member.operator_invoker) == 1 &&
      google_cloud_run_v2_service_iam_member.operator_invoker[0].member == "user:operator@example.com" &&
      google_cloud_run_v2_service_iam_member.operator_invoker[0].role == "roles/run.invoker"
    )
    error_message = "Runtime must grant only the configured operator Cloud Run invocation."
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

run "runtime_requires_conflict_operator" {
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

  expect_failures = [terraform_data.deployment_guard[0]]
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
    folder_transport_operator      = "user:operator@example.com"
  }

  expect_failures = [terraform_data.deployment_guard[0]]
}

run "drive_digest_enables_manual_dry_run_job" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    drive_runtime_image            = "me-west1-docker.pkg.dev/inkbridge-plan-test/inkbridge/drive-runtime@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    drive_runtime_operator         = "user:operator@example.com"
    drive_boox_folder_id           = "boox-folder-id"
    drive_supernote_folder_id      = "supernote-folder-id"
  }

  assert {
    condition     = output.drive_runtime_stage == "dry-run"
    error_message = "An immutable Drive image must select dry-run unless apply mode is separately acknowledged."
  }

  assert {
    condition = (
      length(google_cloud_run_v2_job.drive_runtime) == 1 &&
      google_cloud_run_v2_job.drive_runtime[0].template[0].task_count == 1 &&
      google_cloud_run_v2_job.drive_runtime[0].template[0].parallelism == 1 &&
      google_cloud_run_v2_job.drive_runtime[0].template[0].template[0].max_retries == 0 &&
      google_cloud_run_v2_job.drive_runtime[0].template[0].template[0].timeout == "900s" &&
      length(google_cloud_run_v2_job.drive_runtime[0].template[0].template[0].containers[0].args) == 0
    )
    error_message = "The default Drive job must be one non-retrying, non-mutating task."
  }

  assert {
    condition = {
      for item in google_cloud_run_v2_job.drive_runtime[0].template[0].template[0].containers[0].env :
      item.name => item.value
      } == {
      INKBRIDGE_DRIVE_BOOX_FOLDER_ID       = "boox-folder-id"
      INKBRIDGE_DRIVE_CHECKPOINT_ID        = "primary"
      INKBRIDGE_DRIVE_OAUTH_CLIENT_SECRET  = "inkbridge-drive-oauth-client"
      INKBRIDGE_DRIVE_REFRESH_TOKEN_SECRET = "inkbridge-drive-refresh-token"
      INKBRIDGE_DRIVE_SUPERNOTE_FOLDER_ID  = "supernote-folder-id"
      INKBRIDGE_FIRESTORE_DATABASE         = "(default)"
      INKBRIDGE_GCP_PROJECT                = "inkbridge-plan-test"
      INKBRIDGE_GCS_BUCKET                 = "inkbridge-plan-test-sync"
    }
    error_message = "The Drive job must receive exact folder, checkpoint, secret-container, and broker-store settings."
  }

  assert {
    condition = (
      length(google_cloud_run_v2_job_iam_member.drive_runtime_operator) == 1 &&
      google_cloud_run_v2_job_iam_member.drive_runtime_operator[0].member == "user:operator@example.com" &&
      google_cloud_run_v2_job_iam_member.drive_runtime_operator[0].role == "roles/run.invoker"
    )
    error_message = "Only the configured operator may execute the private Drive job."
  }
}

run "drive_job_requires_operator_and_distinct_folders" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    drive_runtime_image            = "me-west1-docker.pkg.dev/inkbridge-plan-test/inkbridge/drive-runtime@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    drive_boox_folder_id           = "same-folder-id"
    drive_supernote_folder_id      = "same-folder-id"
  }

  expect_failures = [terraform_data.drive_runtime_guard[0]]
}

run "foreign_drive_image_digest_is_rejected" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    drive_runtime_image            = "me-west1-docker.pkg.dev/another-project/inkbridge/drive-runtime@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    drive_runtime_operator         = "user:operator@example.com"
    drive_boox_folder_id           = "boox-folder-id"
    drive_supernote_folder_id      = "supernote-folder-id"
  }

  expect_failures = [terraform_data.drive_runtime_guard[0]]
}

run "drive_secrets_must_be_distinct" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    drive_oauth_client_secret_id   = "same-secret"
    drive_refresh_token_secret_id  = "same-secret"
  }

  expect_failures = [terraform_data.drive_runtime_guard[0]]
}

run "drive_apply_requires_second_acknowledgement" {
  command = plan

  variables {
    enable_deployment              = true
    deployment_acknowledgement     = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                     = "inkbridge-plan-test"
    project_number                 = "123456789012"
    region                         = "me-west1"
    bucket_name                    = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name = "inkbridge-plan-test-build-source"
    drive_runtime_image            = "me-west1-docker.pkg.dev/inkbridge-plan-test/inkbridge/drive-runtime@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    drive_runtime_operator         = "user:operator@example.com"
    drive_boox_folder_id           = "boox-folder-id"
    drive_supernote_folder_id      = "supernote-folder-id"
    drive_runtime_apply_mode       = true
  }

  expect_failures = [terraform_data.drive_runtime_guard[0]]
}

run "acknowledged_drive_apply_adds_only_apply_argument" {
  command = plan

  variables {
    enable_deployment                   = true
    deployment_acknowledgement          = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
    project_id                          = "inkbridge-plan-test"
    project_number                      = "123456789012"
    region                              = "me-west1"
    bucket_name                         = "inkbridge-plan-test-sync"
    cloud_build_source_bucket_name      = "inkbridge-plan-test-build-source"
    drive_runtime_image                 = "me-west1-docker.pkg.dev/inkbridge-plan-test/inkbridge/drive-runtime@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    drive_runtime_operator              = "user:operator@example.com"
    drive_boox_folder_id                = "boox-folder-id"
    drive_supernote_folder_id           = "supernote-folder-id"
    drive_runtime_apply_mode            = true
    drive_runtime_apply_acknowledgement = "I_UNDERSTAND_DRIVE_APPLY_MUTATES_SYNC_STATE"
  }

  assert {
    condition = (
      output.drive_runtime_stage == "apply" &&
      google_cloud_run_v2_job.drive_runtime[0].template[0].template[0].containers[0].args == tolist(["--apply"])
    )
    error_message = "Only the separately acknowledged apply stage may add --apply."
  }
}
