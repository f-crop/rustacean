// REQ-FE-02: email verification surface. Two flows:
//   1. Token arrives on the URL (?token=...) → submit it automatically and
//      route to /login on success.
//   2. No token yet → user pastes one from their email, or stays on the page
//      while we poll /v1/me every 5s. As soon as the server reports the email
//      verified, we route to /repos.
import { useEffect, useMemo, useRef } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { toast } from "sonner";
import { useMe, useResendVerification, useVerifyEmail } from "@/api";
import { AuthLayout } from "@/components/auth/AuthLayout";
import { Field } from "@/components/ui/Field";
import { SubmitButton } from "@/components/ui/SubmitButton";
import { formatApiError } from "@/lib/errors/api";
import { routes } from "@/lib/routes";
import {
  verifyEmailFormSchema,
  type VerifyEmailFormValues,
} from "@/lib/validation/auth";

const ME_POLL_INTERVAL_MS = 5_000;

export function VerifyEmailPage(): JSX.Element {
  const search = useSearch({ from: routes.verifyEmail });
  const initialToken = search.token ?? "";
  const navigate = useNavigate();
  const verifyEmail = useVerifyEmail();
  const resendVerification = useResendVerification();
  const autoSubmitRef = useRef(false);

  const me = useMe({
    refetchInterval: ME_POLL_INTERVAL_MS,
    retry: false,
    refetchOnWindowFocus: false,
  });

  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
    reset,
  } = useForm<VerifyEmailFormValues>({
    resolver: zodResolver(verifyEmailFormSchema),
    defaultValues: { token: initialToken },
  });

  const submitToken = useMemo(
    () =>
      handleSubmit(async (values) => {
        try {
          await verifyEmail.mutateAsync({ token: values.token });
          toast.success("Email verified — please sign in.");
          reset({ token: "" });
          void navigate({ to: routes.login, replace: true });
        } catch (error) {
          toast.error(
            formatApiError(error, "We couldn't verify that token."),
          );
        }
      }),
    [handleSubmit, navigate, reset, verifyEmail],
  );

  useEffect(() => {
    if (initialToken && !autoSubmitRef.current) {
      autoSubmitRef.current = true;
      void submitToken();
    }
  }, [initialToken, submitToken]);

  useEffect(() => {
    if (me.data?.user.email_verified === true) {
      toast.success("Email verified.");
      void navigate({ to: routes.repos, replace: true });
    }
  }, [me.data, navigate]);

  return (
    <AuthLayout
      title="Verify your email"
      subtitle="Open the verification email and either click the link or paste the code below. We'll keep watching for confirmation in the background."
      footer={
        <>
          <Link to={routes.login}>Back to sign in</Link>
          <Link to={routes.signup}>Use a different email</Link>
        </>
      }
    >
      <form className="auth-form" onSubmit={submitToken} noValidate>
        <Field
          label="Verification token"
          autoComplete="one-time-code"
          placeholder="Paste the token from the email"
          {...(errors.token?.message ? { error: errors.token.message } : {})}
          {...register("token")}
        />
        <SubmitButton isLoading={isSubmitting} loadingLabel="Verifying…">
          Verify email
        </SubmitButton>
      </form>
      <button
        type="button"
        className="auth-link-button"
        disabled={resendVerification.isPending || !me.data?.user.email}
        onClick={async () => {
          const email = me.data?.user.email;
          if (!email) return;
          try {
            await resendVerification.mutateAsync({ email });
            toast.success("Verification email resent — check your inbox.");
          } catch {
            toast.error("Couldn't resend the verification email. Try again.");
          }
        }}
      >
        {resendVerification.isPending
          ? "Sending…"
          : "Resend verification email"}
      </button>
      <p className="auth-status">
        Waiting for verification… we check every 5 seconds.
      </p>
    </AuthLayout>
  );
}
