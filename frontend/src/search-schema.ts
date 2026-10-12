import { z } from "zod";

export function createSearchSchema(allowedCategories: string[]) {
    return z.object({
        location: z.string()
            .trim()
            .min(1, "Enter a city or town.")
            .max(100, "Location must be 100 characters or fewer."),
        category: z.string().min(1, "Choose a business category."),
    }).superRefine(({ category }, context) => {
        if (category && !allowedCategories.includes(category)) {
            context.addIssue({
                code: "custom",
                path: ["category"],
                message: "Choose a category from the list.",
            });
        }
    });
}

export type SearchFormValues = z.infer<ReturnType<typeof createSearchSchema>>;
