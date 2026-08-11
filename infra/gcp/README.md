# InkBridge Google Cloud deployment

This configuration is inert by default. `enable_deployment = false` defines
zero managed resources. Enabling it also requires the exact acknowledgement:

```hcl
enable_deployment          = true
deployment_acknowledgement = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
```

CI runs formatting, `init -backend=false`, validation, and credential-free
plans. It never runs `apply`, creates a project, links billing, or deploys a
resource.

## Why deployment has two stages

Cloud Run requires an existing container image, while Artifact Registry and
Cloud Build must exist before that image can be built. The same Terraform
configuration therefore has two explicit enabled stages:

1. **Bootstrap** — `cloud_run_image = ""`. Enables only the bootstrap APIs and
   creates the private versioned data bucket, Firestore database, runtime
   service account and IAM, and regional Artifact Registry repository. Cloud
   Run and Eventarc are deliberately absent.
2. **Runtime** — set `cloud_run_image` to the immutable Artifact Registry
   `@sha256:` URI produced by Cloud Build. Adds the private Cloud Run service,
   Eventarc service account/IAM, runtime APIs, and finalized-object trigger.

This breaks the image/repository cycle without deploying a placeholder image.

## Remote state is mandatory for real applies

The partial `gcs` backend prevents a real deployment from silently relying on
fragile local state. Create one private, versioned state bucket before any real
plan or apply (this bootstrap bucket is intentionally outside the main state):

```text
gcloud storage buckets create gs://STATE_BUCKET \
  --project=PROJECT_ID \
  --location=REGION \
  --uniform-bucket-level-access \
  --public-access-prevention

gcloud storage buckets update gs://STATE_BUCKET --versioning

terraform init -reconfigure \
  -backend-config="bucket=STATE_BUCKET" \
  -backend-config="prefix=inkbridge/cloud-runtime"
```

Never commit `.tfvars`, state files, access tokens, service-account keys, or
saved plans. Copy `terraform.tfvars.example` to a private `.tfvars` file.

## Reviewed deployment sequence

1. Select an isolated project, billing account, region, and unique bucket
   names. Set a budget alert if desired; it is not a hard spending cap.
2. Create and configure the private remote-state bucket.
3. Save and inspect the bootstrap plan. Apply only that exact saved plan.
4. Build a commit-tagged Linux amd64 image:

   ```text
   gcloud builds submit . \
     --project=PROJECT_ID \
     --region=REGION \
     --config=cloudbuild.runtime.yaml \
     --service-account=projects/PROJECT_ID/serviceAccounts/inkbridge-builder@PROJECT_ID.iam.gserviceaccount.com \
     --substitutions=_IMAGE=REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime:GIT_SHA
   ```

5. Resolve the pushed tag to its immutable digest:

   ```text
   gcloud artifacts docker images describe \
     REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime:GIT_SHA \
     --format="value(image_summary.digest)"
   ```

6. Set `cloud_run_image` to the same image path with `:GIT_SHA` replaced by
   `@sha256:...`. Save and inspect a fresh runtime plan.
7. Apply only that exact saved runtime plan, then test registration and one
   finalized update from each device folder.

The Artifact Registry repository keeps the five most recent versions and
deletes untagged versions older than seven days. Cloud Run remains private,
uses zero minimum instances, one maximum instance, and concurrency one.

## Planned resources

The enabled stages manage:

- required project APIs (budget API only when a nonzero budget is requested);
- private regional Artifact Registry repository;
- private versioned device-data bucket;
- Firestore Native database with deletion protection and point-in-time recovery;
- least-scope build, runtime, and Eventarc service accounts/IAM;
- private Cloud Run broker; and
- Eventarc finalized-object trigger.
